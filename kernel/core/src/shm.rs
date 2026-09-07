//! Shared file mappings: the page cache nk does not have, in miniature.
//!
//! A `MAP_PRIVATE` file mapping is a snapshot: Linux reads the file into
//! nk-owned pages once, and the process never talks to the filesystem again.
//! `MAP_SHARED` is a different contract. The pages have to be the same memory
//! for every process holding the mapping -- a `wl_shm` buffer written by a
//! client and read by the compositor, a `memfd` passed over `SCM_RIGHTS` --
//! and a write has to reach the file when the mapping is unmapped or `msync`
//! is called.
//!
//! The pool below is that contract, cut down to what the desktop needs. Each
//! shared region is keyed by the file it came from *and* the offset, holds
//! the physical pages once, and counts how many mappings point at them. The
//! first mapper fills the pages with `preadv` and the last one out writes
//! dirty pages back with `pwritev` through an fd the region owns (a duplicate
//! of the first mapper's -- the mapper may have closed or exited since).
//! `msync` writes them back sooner.
//!
//! What this is not: coherence with later file writes through other
//! descriptors, `MAP_SHARED` on a device or DRM dumb buffer (those pages are
//! Linux's, not nk's -- pinning them is separate work), or demand paging.
//! Every page is still mapped eagerly at `mmap` time.

use crate::frames;
#[cfg(nk_lkl)]
use crate::lkl;
use crate::paging;
use alloc::vec::Vec;

/// A shared region: one file range, one set of pages, however many mappings.
struct Region {
    /// What file this is. `(dev, ino)` from `fstat`, which is what makes two
    /// opens of the same file the same region -- and what makes two different
    /// files different ones even when they have the same size.
    key: (u64, u64),
    /// File offset the region starts at. A region is contiguous in the file
    /// by construction, so page `i` belongs at `base + i*4096`.
    base: u64,
    /// An owned fd for writeback, duplicated from the first mapper's. A
    /// region outlives any particular mapping's fd, so the pool keeps its
    /// own. -1 when the duplication failed: the mapping still works, only
    /// writeback falls back to the caller's fd.
    fd: i64,
    /// File size at map time. Pages at or past it read as zero and are never
    /// written back, which is the `ftruncate`-then-`mmap` shape `memfd`
    /// users depend on: the file is 4KB because someone said so, not because
    /// it has 4KB of bytes in it yet.
    size: u64,
    /// One entry per page of the region. Always `Some`: past-EOF pages hold
    /// zeroed frames with no file behind them (and writeback skips them by
    /// comparing against `size`).
    pages: Vec<*mut u8>,
    /// How many mappings point here. The pages are freed when this reaches
    /// zero -- after writing back, if dirty.
    refs: usize,
    /// Whether any page may have been written through a writable mapping.
    /// Set at map time for writable mappings -- there is no fault path yet
    /// to observe the first write -- because over-writing back is correct
    /// and under-writing is not.
    dirty: bool,
}

static mut REGIONS: Vec<Region> = Vec::new();

/// Identity of a region: the key, or an error that is already an errno.
///
/// Without Linux there is no file to key on: anonymous-shared is the only
/// shape that works, and it never reaches here.
#[cfg(nk_lkl)]
fn file_key(fd: i64) -> Result<(u64, u64), i64> {
    // `fstat` is 80 on asm-generic. The struct Linux fills is 128 bytes;
    // dev is at 8, ino at 16 -- see `stat64` in any libc's `bits/stat.h`.
    let mut st = [0u8; 128];
    let rc = lkl::syscall(80, [fd, st.as_mut_ptr() as i64, 0, 0, 0, 0]);
    if rc < 0 {
        return Err(rc);
    }
    let dev = u64::from_le_bytes(st[8..16].try_into().unwrap());
    let ino = u64::from_le_bytes(st[16..24].try_into().unwrap());
    Ok((dev, ino))
}

/// File size from the same `fstat` buffer: `st_size` at 48.
fn file_size(st: &[u8; 128]) -> u64 {
    u64::from_le_bytes(st[48..56].try_into().unwrap())
}

/// Map `len` bytes of `fd` at `at`, shared. `len` is page-rounded, `offset`
/// page-aligned, `at` chosen by the caller. Fills from the file on first map,
/// reuses the pool's pages when the region already exists.
///
/// Returns the region index, so `munmap` and `msync` can find it again.
#[cfg(nk_lkl)]
pub fn map_shared(
    fd: i64,
    offset: u64,
    len: u64,
    at: u64,
    writable: bool,
    executable: bool,
    prot_none: bool,
) -> Result<usize, i64> {
    let key = file_key(fd)?;
    let mut st = [0u8; 128];
    let rc = lkl::syscall(80, [fd, st.as_mut_ptr() as i64, 0, 0, 0, 0]);
    if rc < 0 {
        return Err(rc);
    }
    let size = file_size(&st);

    let regions = unsafe { &mut *(&raw mut REGIONS) };
    // Same file *and* same offset: a region is contiguous in the file, so a
    // different offset is a different region even for the same (dev, ino).
    // A shorter existing region cannot serve a longer mapping either.
    let idx = if let Some(i) = regions
        .iter()
        .position(|r| r.key == key && r.base == offset && r.pages.len() as u64 * 4096 >= len)
    {
        regions[i].refs += 1;
        if writable {
            regions[i].dirty = true;
        }
        i
    } else {
        // A new region. Every page gets a frame now, eagerly, like
        // everything else nk maps. File-backed pages are filled with
        // `preadv` in 256-page batches; pages at or past end-of-file keep
        // their zeroed frames and are never written back.
        let npages = (len / 4096) as usize;
        let mut pages: Vec<*mut u8> = Vec::with_capacity(npages);
        for _ in 0..npages {
            let Some(page) = frames::alloc() else {
                for p in pages {
                    unsafe { frames::free(p) };
                }
                return Err(-12);
            };
            // `alloc` zeroes, which is what past-EOF requires.
            pages.push(page);
        }
        // Read the file-backed prefix. The frames are scattered but
        // contiguous in the file, so one `preadv` per batch with one iovec
        // per page -- the same shape as the private path. A short read is
        // fine: the rest is zeros, which it already is.
        const BATCH: usize = 256;
        #[repr(C)]
        struct IoVec {
            base: u64,
            len: u64,
        }
        let mut done = 0usize;
        while done < npages {
            let mut n = 0usize;
            while done + n < npages && n < BATCH && offset + ((done + n) as u64) * 4096 < size {
                n += 1;
            }
            if n == 0 {
                break;
            }
            let iov: Vec<IoVec> = pages[done..done + n]
                .iter()
                .map(|p| IoVec {
                    base: *p as u64,
                    len: 4096,
                })
                .collect();
            let rc = lkl::syscall(
                69,
                [
                    fd,
                    iov.as_ptr() as i64,
                    iov.len() as i64,
                    match offset.checked_add((done as u64) * 4096) {
                        Some(v) if v <= i64::MAX as u64 => v as i64,
                        _ => -1,
                    },
                    0,
                    0,
                ],
            );
            if rc < 0 {
                for p in pages {
                    unsafe { frames::free(p) };
                }
                return Err(rc);
            }
            done += n;
        }
        // The region owns its fd: duplicated, so the mapper may close or
        // exit without taking writeback with it. `fcntl` here is 25, which
        // on this ABI is `fcntl64` -- the only fcntl there is -- and
        // `F_DUPFD` is 0: lowest free at or above the floor. That is the
        // semantics this needs, unlike `dup3` onto a fixed number, which is
        // refused when that number is taken (every region after the first).
        // The floor sits above the 0..64 scan `inherit_fds` walks, so owned
        // fds never leak into children -- and below the 1024 file limit, so
        // the duplication cannot fail for want of room.
        let owned = lkl::syscall(25, [fd, 0, 512, 0, 0, 0]);
        regions.push(Region {
            key,
            base: offset,
            fd: if owned < 0 { -1 } else { owned },
            size,
            pages,
            refs: 1,
            dirty: writable,
        });
        regions.len() - 1
    };

    // Map the pool's pages into the caller: the same physical pages every
    // holder of the region sees, which is the whole of the contract.
    // PROT_NONE (`prot == 0`) is what `protect_user_none` is for -- it
    // clears the access flag so the pages fault on any access. PROT_READ
    // must not come here: it is a valid mapping with AP_RO_ANY already,
    // and clearing it turns a readable page into an EL0 permission fault
    // (DFSC 0b001111; measured 2026-09-08 on a wl_shm pool read).
    let root = crate::user::current_ttbr0_pub();
    for (i, page) in unsafe { (&*(&raw const REGIONS))[idx].pages.iter().enumerate() } {
        unsafe {
            paging::map_user_permissions(
                root,
                at + i as u64 * 4096,
                *page as u64,
                4096,
                executable,
                writable,
            );
        }
    }
    if !writable && !executable && prot_none {
        unsafe {
            paging::protect_user_none(root, at, len);
        }
    }
    Ok(idx)
}

/// Anonymous shared: same pages for every holder that maps them, no file
/// behind them. The region is keyed nowhere -- it is found by the caller's
/// VMA record, not by lookup -- so this just allocates and maps.
pub fn map_anonymous_shared(len: u64, at: u64, writable: bool, executable: bool, prot_none: bool) -> usize {
    let npages = (len / 4096) as usize;
    let mut pages: Vec<*mut u8> = Vec::with_capacity(npages);
    for _ in 0..npages {
        let Some(page) = frames::alloc() else {
            for p in pages {
                unsafe { frames::free(p) };
            }
            // Out of memory with half a region allocated: leak nothing, map
            // nothing. The caller treats `usize::MAX` as `-ENOMEM`.
            return usize::MAX;
        };
        pages.push(page);
    }
    let regions = unsafe { &mut *(&raw mut REGIONS) };
    // Anonymous regions share no key: (dev, ino) of nothing. Key them
    // (0, base-address-of-pool-entry) so they never collide with a file.
    let key = (0u64, regions.len() as u64 | (1 << 63));
    regions.push(Region {
        key,
        base: 0,
        fd: -1,
        size: u64::MAX,
        pages,
        refs: 1,
        dirty: false,
    });
    let idx = regions.len() - 1;
    let root = crate::user::current_ttbr0_pub();
    for (i, page) in unsafe { (&*(&raw const REGIONS))[idx].pages.iter().enumerate() } {
        unsafe {
            paging::map_user_permissions(
                root,
                at + i as u64 * 4096,
                *page as u64,
                4096,
                executable,
                writable,
            );
        }
    }
    if !writable && !executable && prot_none {
        unsafe {
            paging::protect_user_none(root, at, len);
        }
    }
    idx
}

/// Write a region's file-backed pages back through `pwritev` (70 on
/// asm-generic), in 256-page batches. Pages at or past the file size are
/// skipped: the file has nothing there. Clears the dirty flag.
#[cfg(nk_lkl)]
fn writeback(idx: usize, fd_hint: i64) {
    #[repr(C)]
    struct IoVec {
        base: u64,
        len: u64,
    }
    let (fd, base, size, pages) = unsafe {
        let Some(r) = (&*(&raw const REGIONS)).get(idx) else {
            return;
        };
        if !r.dirty {
            return;
        }
        let owned = if r.fd >= 0 { r.fd } else { fd_hint };
        (owned, r.base, r.size, r.pages.clone())
    };
    if fd < 0 {
        return;
    }
    // Contiguous batches: the region is contiguous in the file by
    // construction, so a run of file-backed pages is one `pwritev` -- or,
    // failing that, one `pwrite64` per page. `pwritev` first: one syscall
    // per 256 pages rather than 256 of them.
    let mut i = 0;
    while i < pages.len() {
        if base + i as u64 * 4096 >= size {
            i += 1;
            continue;
        }
        let start = i;
        while i < pages.len() && i - start < 256 && base + i as u64 * 4096 < size {
            i += 1;
        }
        let iov: Vec<IoVec> = pages[start..i]
            .iter()
            .map(|p| IoVec {
                base: *p as u64,
                len: 4096,
            })
            .collect();
        let off = base + start as u64 * 4096;
        let rc = lkl::syscall(
            70,
            [
                fd,
                iov.as_ptr() as i64,
                iov.len() as i64,
                off as i64,
                0,
                0,
            ],
        );
        if rc < 0 {
            // `pwritev` is not one Linux must have for LKL's shape -- fall
            // back to a `pwrite64` (68) per page, which every configuration
            // answers. A short write is retried; an error stops the batch
            // but keeps what landed, and the region stays dirty.
            let mut ok = true;
            for (j, page) in pages[start..i].iter().enumerate() {
                let mut at = off + j as u64 * 4096;
                let mut left = 4096u64;
                while left > 0 {
                    let n = lkl::syscall(
                        68,
                        [fd, *page as i64 + (4096 - left) as i64, left as i64, at as i64, 0, 0],
                    );
                    if n <= 0 {
                        ok = false;
                        break;
                    }
                    at += n as u64;
                    left -= n as u64;
                }
                if !ok {
                    break;
                }
            }
            if !ok {
                return;
            }
        }
    }
    unsafe {
        if let Some(r) = (&mut *(&raw mut REGIONS)).get_mut(idx) {
            r.dirty = false;
        }
    }
}

/// Write back and drop one reference. When the last mapping goes, free the
/// pages and close the owned fd.
#[cfg(nk_lkl)]
pub fn release(idx: usize, fd_hint: i64) {
    writeback(idx, fd_hint);
    unsafe {
        let regions = &mut *(&raw mut REGIONS);
        let Some(r) = regions.get_mut(idx) else {
            return;
        };
        if r.refs > 0 {
            r.refs -= 1;
        }
        if r.refs > 0 {
            return;
        }
        for p in r.pages.drain(..) {
            frames::free(p);
        }
        if r.fd >= 0 {
            lkl::syscall(57, [r.fd, 0, 0, 0, 0, 0]);
            r.fd = -1;
        }
    }
}

/// `msync`: write back every dirty region. The address range is accepted
/// and not filtered on: every region is small, writes complete before the
/// call returns, and `MS_ASYNC` vs `MS_SYNC` differ in when the write
/// completes, which here is already. An `msync` over no shared mappings is
/// success.
#[cfg(nk_lkl)]
pub fn msync_all(fd_hint: i64) -> i64 {
    let n = unsafe { (&*(&raw const REGIONS)).len() };
    for idx in 0..n {
        writeback(idx, fd_hint);
    }
    0
}

/// Release an anonymous-shared region: no writeback, just unref and free.
pub fn release_anon(idx: usize) {
    unsafe {
        let regions = &mut *(&raw mut REGIONS);
        let Some(r) = regions.get_mut(idx) else {
            return;
        };
        if r.refs > 0 {
            r.refs -= 1;
        }
        if r.refs > 0 {
            return;
        }
        for p in r.pages.drain(..) {
            frames::free(p);
        }
    }
}

// Without Linux there is no file to share: file-backed shared mappings,
// writeback and msync refuse with -ENOSYS. Anonymous-shared above still
// works -- it is only frames and page tables.
#[cfg(not(nk_lkl))]
pub fn map_shared(_fd: i64, _offset: u64, _len: u64, _at: u64, _writable: bool, _executable: bool) -> Result<usize, i64> {
    Err(-38)
}

#[cfg(not(nk_lkl))]
pub fn release(_idx: usize, _fd_hint: i64) {}

#[cfg(not(nk_lkl))]
pub fn msync_all(_fd_hint: i64) -> i64 {
    -38
}
