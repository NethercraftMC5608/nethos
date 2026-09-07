//! A bounded ELF64 loader description, independent of the MMU.
//! Reject unsupported layouts before allocating or modifying any page tables.
extern crate alloc;
use alloc::vec::Vec;

#[derive(Debug)]
pub struct Segment {
    pub address: u64,
    pub offset: usize,
    pub filesz: usize,
    pub memsz: usize,
    pub executable: bool,
    pub writable: bool,
}
#[derive(Debug)]
pub struct Image {
    pub entry: u64,
    pub segments: Vec<Segment>,
    /// Where the program headers are in the file, and how many. A libc reads
    /// its own headers at startup -- for TLS, for the stack-guard flag, for
    /// `dl_iterate_phdr` -- so it is handed their *address* in AT_PHDR, and
    /// that can only be worked out from the segment that happens to contain
    /// them.
    pub phoff: usize,
    pub phnum: usize,
    pub interpreter: Option<alloc::vec::Vec<u8>>,
    pub load_bias: u64,
}
fn n(bytes: &[u8], off: usize, len: usize) -> Result<u64, &'static str> {
    let s = bytes
        .get(off..off.checked_add(len).ok_or("overflow")?)
        .ok_or("truncated ELF")?;
    Ok(s.iter()
        .enumerate()
        .fold(0, |v, (i, b)| v | ((*b as u64) << (8 * i))))
}
pub fn parse(b: &[u8], low: u64, high: u64) -> Result<Image, &'static str> {
    parse_at(b, low, high, low)
}
pub fn parse_at(b: &[u8], low: u64, high: u64, bias: u64) -> Result<Image, &'static str> {
    let kind = n(b, 16, 2)?;
    let bias = if kind == 3 { bias } else { 0 };
    if b.get(..7) != Some(b"\x7fELF\x02\x01\x01")
        || !matches!(kind, 2 | 3)
        || n(b, 18, 2)? != 183
        || n(b, 20, 4)? != 1
        || n(b, 52, 2)? != 64
        || n(b, 54, 2)? != 56
    {
        return Err("expected little-endian AArch64 ELF64");
    }
    let entry = n(b, 24, 8)?.checked_add(bias).ok_or("entry overflow")?;
    let phoff = usize::try_from(n(b, 32, 8)?).map_err(|_| "overflow")?;
    let count = n(b, 56, 2)? as usize;
    if count == 0 || count > 32 {
        return Err("invalid program header count");
    }
    let mut interpreter = None;
    let mut segments: Vec<Segment> = Vec::new();
    for i in 0..count {
        let p = phoff.checked_add(i * 56).ok_or("overflow")?;
        b.get(p..p.checked_add(56).ok_or("overflow")?)
            .ok_or("truncated headers")?;
        let kind = n(b, p, 4)?;
        if kind == 3 {
            if interpreter.is_some() {
                return Err("multiple interpreters");
            }
            let offset = n(b, p + 8, 8)?;
            let size = n(b, p + 32, 8)?;
            if size < 2 || size > 4096 {
                return Err("invalid interpreter");
            }
            let end = offset.checked_add(size).ok_or("interpreter overflow")?;
            let path = b
                .get(offset as usize..end as usize)
                .ok_or("truncated interpreter")?;
            if path[0] != b'/' || path.last() != Some(&0) || path[..path.len() - 1].contains(&0) {
                return Err("invalid interpreter path");
            }
            interpreter = Some(path.to_vec());
        }
        if kind != 1 {
            continue;
        }
        let flags = n(b, p + 4, 4)?;
        let offset = n(b, p + 8, 8)?;
        let address = n(b, p + 16, 8)?
            .checked_add(bias)
            .ok_or("address overflow")?;
        let filesz = n(b, p + 32, 8)?;
        let memsz = n(b, p + 40, 8)?;
        let align = n(b, p + 48, 8)?;
        let end = address.checked_add(memsz).ok_or("segment overflow")?;
        if filesz > memsz
            || offset.checked_add(filesz).ok_or("file overflow")? > b.len() as u64
            || address < low
            || end > high
            || flags & 3 == 3
            || flags & 4 == 0
            || (align > 1 && (!align.is_power_of_two() || address % align != offset % align))
        {
            return Err("invalid load segment");
        }
        if memsz == 0 {
            continue;
        }
        let page_start = address & !4095;
        let page_end = end.checked_add(4095).ok_or("overflow")? & !4095;
        if segments.iter().any(|s| {
            page_start < ((s.address + s.memsz as u64 + 4095) & !4095)
                && page_end > (s.address & !4095)
        }) {
            return Err("overlapping load pages");
        }
        segments.push(Segment {
            address,
            offset: offset as usize,
            filesz: filesz as usize,
            memsz: memsz as usize,
            executable: flags & 1 != 0,
            writable: flags & 2 != 0,
        });
    }
    if entry % 4 != 0
        || !segments
            .iter()
            .any(|s| s.executable && entry >= s.address && entry < s.address + s.filesz as u64)
    {
        return Err("entry is not executable file data");
    }
    Ok(Image {
        entry,
        segments,
        phoff,
        phnum: count,
        interpreter,
        load_bias: bias,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(b: &mut [u8], o: usize, v: u64, len: usize) {
        b[o..o + len].copy_from_slice(&v.to_le_bytes()[..len]);
    }
    fn fixture() -> Vec<u8> {
        let mut b = alloc::vec![0; 4100];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (o, v, l) in [
            (16, 2, 2),
            (18, 183, 2),
            (20, 1, 4),
            (24, 0x1000, 8),
            (32, 64, 8),
            (52, 64, 2),
            (54, 56, 2),
            (56, 1, 2),
            (64, 1, 4),
            (68, 5, 4),
            (72, 4096, 8),
            (80, 0x1000, 8),
            (96, 4, 8),
            (104, 4096, 8),
            (112, 4096, 8),
        ] {
            put(&mut b, o, v, l);
        }
        b
    }
    #[test]
    fn valid_bss() {
        let i = parse(&fixture(), 0x1000, 0x10000).unwrap();
        assert_eq!(i.entry, 0x1000);
        assert_eq!(i.segments[0].memsz, 4096);
        assert_eq!(i.segments[0].filesz, 4);
        assert!(!i.segments[0].writable);
    }
    #[test]
    fn all_truncations_rejected() {
        let b = fixture();
        for len in 0..b.len() {
            assert!(parse(&b[..len], 0x1000, 0x10000).is_err());
        }
    }
    #[test]
    fn invalid_headers_and_segments() {
        for (off, value, len) in [
            (18, 62, 2),
            (64, 3, 4),
            (64, 2, 4),
            (64, 7, 4),
            (68, 7, 4),
            (32, u64::MAX, 8),
            (72, u64::MAX, 8),
            (104, 2, 8),
            (104, u64::MAX, 8),
            (80, 0, 8),
            (80, 0x10000, 8),
            (112, 3, 8),
            (24, 0x1004, 8),
            (24, 0x1001, 8),
            (56, 33, 2),
            (56, 0, 2),
        ] {
            let mut b = fixture();
            put(&mut b, off, value, len);
            assert!(
                parse(&b, 0x1000, 0x10000).is_err(),
                "field {off}, value {value}"
            );
        }
    }
    #[test]
    fn pie_is_rebased() {
        let mut b = fixture();
        put(&mut b, 16, 3, 2);
        let image = parse_at(&b, 0x1000, 0x10000, 0x4000).unwrap();
        assert_eq!(image.entry, 0x5000);
        assert_eq!(image.segments[0].address, 0x5000);
        assert!(parse_at(&b, 0x1000, 0x10000, u64::MAX).is_err());
    }
    #[test]
    fn interpreter_path_is_bounded_and_nul_terminated() {
        let mut b = fixture();
        put(&mut b, 56, 2, 2);
        put(&mut b, 120, 3, 4);
        put(&mut b, 128, 200, 8);
        put(&mut b, 152, 8, 8);
        b[200..208].copy_from_slice(b"/ld.so\0\0");
        assert!(parse(&b, 0x1000, 0x10000).is_err());
        put(&mut b, 152, 7, 8);
        assert_eq!(
            parse(&b, 0x1000, 0x10000).unwrap().interpreter.unwrap(),
            b"/ld.so\0"
        );
        b[206] = b'x';
        assert!(parse(&b, 0x1000, 0x10000).is_err());
    }
    #[test]
    fn overlapping_pages_rejected() {
        let mut b = fixture();
        let ph = b[64..120].to_vec();
        b[120..176].copy_from_slice(&ph);
        put(&mut b, 56, 2, 2);
        assert!(parse(&b, 0x1000, 0x10000).is_err());
    }
}
