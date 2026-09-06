//! The `newc` cpio archive, which is how a Linux system's first filesystem
//! arrives.
//!
//! An initial ramdisk is a cpio archive because cpio is the format you can
//! unpack without already having a filesystem: it is a flat stream of
//! (header, name, data) with no index, no compression and no seeking. That is
//! the whole reason Linux uses it for initramfs rather than tar or an image,
//! and it is the reason this file is ninety lines rather than a driver.
//!
//! Only `newc` (magic "070701"), which is what `cpio -H newc` and every
//! initramfs build script produces. The older binary and ASCII formats are
//! not accepted: guessing between them from a stream that might be either is
//! how an archive gets silently misread, and refusing is better.

/// One entry: a name, a mode, and its contents.
pub struct Entry<'a> {
    pub name: &'a str,
    pub mode: u32,
    pub data: &'a [u8],
}

impl Entry<'_> {
    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
    pub fn is_file(&self) -> bool {
        self.mode & 0o170000 == 0o100000
    }
    pub fn is_symlink(&self) -> bool {
        self.mode & 0o170000 == 0o120000
    }
    /// The permission bits, without the type.
    pub fn perms(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// Every field in a newc header is eight ASCII hex digits. Not four bytes of
/// binary: the format is meant to survive being copied through anything that
/// might translate bytes, which is also why it has no endianness.
fn hex8(b: &[u8], at: usize) -> Option<u32> {
    let f = b.get(at..at + 8)?;
    let mut v: u32 = 0;
    for c in f {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        };
        v = v.checked_mul(16)?.checked_add(d as u32)?;
    }
    Some(v)
}

const HEADER: usize = 110;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Call `f` for each entry in order. Stops at `TRAILER!!!`, which is the
/// archive's own end marker -- there is no count anywhere, so a truncated
/// archive is indistinguishable from a complete one except by that name
/// being absent, and this reports that as an error rather than as success.
pub fn each<'a>(archive: &'a [u8], mut f: impl FnMut(Entry<'a>)) -> Result<(), &'static str> {
    let mut at = 0;
    loop {
        let h = archive.get(at..at + HEADER).ok_or("truncated cpio header")?;
        if &h[..6] != b"070701" {
            return Err("not a newc cpio archive");
        }
        let mode = hex8(h, 14).ok_or("bad mode")?;
        let filesize = hex8(h, 54).ok_or("bad size")? as usize;
        let namesize = hex8(h, 94).ok_or("bad name size")? as usize;

        // The name includes its NUL, and both the name and the data are
        // padded to a four-byte boundary *measured from the start of the
        // archive* -- which is why the header length is added back in before
        // aligning rather than after.
        let name_at = at + HEADER;
        let raw = archive
            .get(name_at..name_at + namesize)
            .ok_or("truncated cpio name")?;
        let name = core::str::from_utf8(raw.split_last().ok_or("empty name")?.1)
            .map_err(|_| "cpio name is not utf-8")?;
        let data_at = align4(name_at + namesize);
        let data = archive
            .get(data_at..data_at + filesize)
            .ok_or("truncated cpio data")?;

        if name == "TRAILER!!!" {
            return Ok(());
        }
        // "." is the archive's own root and already exists wherever it is
        // being unpacked; creating it is at best a no-op and at worst an
        // error the caller would have to know to ignore.
        if name != "." {
            f(Entry { name, mode, data });
        }
        at = align4(data_at + filesize);
    }
}
