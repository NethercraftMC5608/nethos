//! The physical frame allocator: 4KB pages, and where they come from.
//!
//! A bump pointer for pages that have never been handed out, and a free list
//! threaded through the pages themselves for ones that have come back. Both
//! operations are O(1) and the whole thing costs two words of metadata.
//!
//! The obvious alternative -- push every page onto a free list at boot -- was
//! rejected for a reason worth writing down: it writes eight bytes to every
//! page in the machine before the kernel does anything, which on a 512MB
//! guest is half a gigabyte of cache-missing stores, and on a real 32GB
//! machine is far worse. The bump pointer costs nothing and reaches the same
//! state lazily.
//!
//! No lock. Every CPU but affinity 0 is parked in boot.s, so there is exactly
//! one caller. SMP bring-up has to add one here, and this comment is the note
//! that it is missing rather than merely absent.

use crate::println;

pub const PAGE: usize = 4096;

/// A region of RAM that has never been allocated from. Fixed capacity: the
/// device tree's memory node is one entry on every machine nk targets, and a
/// fixed array needs no allocator, which is the point.
#[derive(Clone, Copy)]
struct Region {
    next: usize, // the bump pointer
    end: usize,
}

const MAX_REGIONS: usize = 8;

struct Frames {
    regions: [Region; MAX_REGIONS],
    nregions: usize,
    free: *mut FreeFrame,
    total: usize,
    used: usize,
}

/// Written into a frame that is on the free list. The list lives in the frames
/// it describes, so it costs no memory of its own.
struct FreeFrame {
    next: *mut FreeFrame,
}

static mut FRAMES: Frames = Frames {
    regions: [Region { next: 0, end: 0 }; MAX_REGIONS],
    nregions: 0,
    free: core::ptr::null_mut(),
    total: 0,
    used: 0,
};

const fn align_up(n: usize, to: usize) -> usize {
    (n + to - 1) & !(to - 1)
}

/// Give the allocator a range of physical memory. Ends are exclusive, and
/// anything that is not page-aligned is trimmed inwards rather than rounded
/// out over whatever is next to it.
///
/// # Safety
/// The range must be real RAM that nothing else owns.
pub unsafe fn add(start: usize, end: usize) {
    let start = align_up(start, PAGE);
    let end = end & !(PAGE - 1);
    if end <= start {
        return;
    }
    let f = &mut *(&raw mut FRAMES);
    if f.nregions == MAX_REGIONS {
        return;
    }
    f.regions[f.nregions] = Region { next: start, end };
    f.nregions += 1;
    f.total += (end - start) / PAGE;
}

/// One zeroed frame, or None when memory is gone.
///
/// Zeroed unconditionally. Page tables require it, Linux's `kzalloc` sits on
/// it, and a frame that quietly carries a previous owner's data is both a
/// correctness bug and an information leak -- and neither shows up until much
/// later, in something entirely unrelated.
pub fn alloc() -> Option<*mut u8> {
    unsafe {
        let f = &mut *(&raw mut FRAMES);
        let p = if !f.free.is_null() {
            let p = f.free;
            f.free = (*p).next;
            p as *mut u8
        } else {
            let mut got = core::ptr::null_mut();
            for r in &mut f.regions[..f.nregions] {
                if r.next < r.end {
                    got = r.next as *mut u8;
                    r.next += PAGE;
                    break;
                }
            }
            if got.is_null() {
                return None;
            }
            got
        };
        core::ptr::write_bytes(p, 0, PAGE);
        f.used += 1;
        Some(p)
    }
}

/// `n` frames that are contiguous in physical memory.
///
/// Bump-only: the free list holds single frames in no particular order, so it
/// can never satisfy this. That is the right trade -- the only caller is the
/// heap, once, at boot. A driver wanting contiguous DMA memory at runtime is
/// Stage 3's problem and will need a real buddy allocator.
pub fn alloc_contiguous(n: usize) -> Option<*mut u8> {
    unsafe {
        let f = &mut *(&raw mut FRAMES);
        for r in &mut f.regions[..f.nregions] {
            if r.end - r.next >= n * PAGE {
                let p = r.next as *mut u8;
                r.next += n * PAGE;
                f.used += n;
                core::ptr::write_bytes(p, 0, n * PAGE);
                return Some(p);
            }
        }
        None
    }
}

/// # Safety
/// `p` must have come from `alloc` and must not be used again.
pub unsafe fn free(p: *mut u8) {
    let f = &mut *(&raw mut FRAMES);
    let node = p as *mut FreeFrame;
    (*node).next = f.free;
    f.free = node;
    f.used -= 1;
}

pub fn stats() -> (usize, usize) {
    unsafe {
        let f = &*(&raw const FRAMES);
        (f.used, f.total)
    }
}

pub fn report() {
    let (used, total) = stats();
    println!(
        "  frames: {} of {} pages used ({} MiB free)",
        used,
        total,
        (total - used) * PAGE / (1024 * 1024)
    );
}
