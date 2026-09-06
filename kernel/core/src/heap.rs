//! The kernel heap. First fit, with splitting and coalescing.
//!
//! This is what `kmalloc` will sit on, so it has to do the things `kmalloc`
//! callers assume: arbitrary sizes, arbitrary alignments, and free in any
//! order without the arena degrading into confetti. First fit over an
//! address-ordered free list with coalescing on free is the smallest design
//! that does all three, and it is a design that can be read.
//!
//! It is not a slab allocator and makes no attempt to be fast. When a driver
//! is allocating in a hot path and it matters, that will be measurable rather
//! than assumed -- which is the only good reason to write the more complicated
//! one.
//!
//! No lock, for the same reason as `frames`: one CPU is running. SMP needs one
//! here too.

use crate::frames::{self, PAGE};
use crate::println;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

/// A free block, with its bookkeeping stored inside the free space itself.
/// The list is kept sorted by address, which is what makes coalescing a
/// comparison against the neighbours rather than a search.
#[repr(C)]
struct Free {
    size: usize, // the whole block, this header included
    next: *mut Free,
}

/// Written immediately before every allocation. `block`/`size` describe the
/// underlying block rather than the request, because alignment padding means
/// the pointer handed out is not the pointer that has to be given back.
#[repr(C)]
struct Hdr {
    block: *mut u8,
    size: usize,
}

const HDR: usize = core::mem::size_of::<Hdr>();
/// Small enough to be worth splitting off; anything less is left attached to
/// the allocation, because a free block too small to hold its own header
/// cannot go on the list at all.
const MIN_BLOCK: usize = core::mem::size_of::<Free>() * 2;

struct State {
    free: *mut Free,
    total: usize,
    used: usize,
}

static mut HEAP: State = State { free: ptr::null_mut(), total: 0, used: 0 };

/// The allocator is a unit struct and the state is a separate static: a
/// `#[global_allocator]` must be an immutable static, and pretending mutable
/// state is immutable so it can carry the attribute is worse than keeping the
/// two apart and saying why.
pub struct Heap;

// Sound only because nk is single-CPU; see the module note.
unsafe impl Sync for Heap {}

#[global_allocator]
static ALLOCATOR: Heap = Heap;

const fn align_up(n: usize, to: usize) -> usize {
    (n + to - 1) & !(to - 1)
}

/// Take `mib` megabytes of contiguous frames and make a heap of them.
pub fn init(mib: usize) {
    let pages = mib * 1024 * 1024 / PAGE;
    let base = frames::alloc_contiguous(pages).expect("not enough memory for the kernel heap");
    unsafe {
        let h = &mut *(&raw mut HEAP);
        let b = base as *mut Free;
        (*b).size = pages * PAGE;
        (*b).next = ptr::null_mut();
        h.free = b;
        h.total = pages * PAGE;
        h.used = 0;
    }
    println!("  heap:   {} MiB at {:#x}", mib, base as usize);
}

pub fn stats() -> (usize, usize) {
    unsafe {
        let h = &*(&raw const HEAP);
        (h.used, h.total)
    }
}

/// How many blocks the free list holds. The only externally visible evidence
/// that coalescing works: allocate a spread of blocks, free them all, and if
/// this does not come back to one, free() is leaving the arena in pieces.
pub fn free_blocks() -> usize {
    unsafe {
        let h = &*(&raw const HEAP);
        let mut n = 0;
        let mut cur = h.free;
        while !cur.is_null() {
            n += 1;
            cur = (*cur).next;
        }
        n
    }
}

pub fn report() {
    let (used, total) = stats();
    println!("  heap:   {} of {} bytes used", used, total);
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let h = &mut *(&raw mut HEAP);
        // Worst case: the block starts one byte past an aligned address, so
        // the header plus a full alignment's worth of padding is needed before
        // the payload can begin.
        let need = align_up(HDR + layout.align() + layout.size(), 16).max(MIN_BLOCK);

        let mut prev: *mut *mut Free = &raw mut h.free;
        let mut cur = h.free;
        while !cur.is_null() {
            if (*cur).size >= need {
                let block = cur as *mut u8;
                let size = (*cur).size;
                let next = (*cur).next;

                // Unlink first, then carve: the block's own memory is about to
                // be overwritten with the header, so `next` cannot be read
                // from it afterwards.
                *prev = next;

                // Leave the tail on the list if what is left could hold a
                // block of its own. Otherwise it stays with the allocation --
                // a few wasted bytes rather than a free block that cannot be
                // described.
                let take = if size - need >= MIN_BLOCK {
                    let tail = block.add(need) as *mut Free;
                    (*tail).size = size - need;
                    (*tail).next = *prev;
                    *prev = tail;
                    need
                } else {
                    size
                };

                let payload = align_up(block as usize + HDR, layout.align()) as *mut u8;
                let hdr = payload.sub(HDR) as *mut Hdr;
                (*hdr).block = block;
                (*hdr).size = take;
                h.used += take;
                return payload;
            }
            prev = &raw mut (*cur).next;
            cur = (*cur).next;
        }
        // The contract is a null pointer, not a panic: Rust's own allocation
        // machinery turns this into a handle_alloc_error, and a driver that
        // checks its return value deserves to see the failure it checked for.
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let h = &mut *(&raw mut HEAP);
        let hdr = &*(ptr.sub(HDR) as *const Hdr);
        let block = hdr.block;
        let size = hdr.size;
        h.used -= size;

        // Insert in address order, which is what makes the two coalescing
        // checks below possible at all.
        let mut prev: *mut *mut Free = &raw mut h.free;
        let mut cur = h.free;
        while !cur.is_null() && (cur as *mut u8) < block {
            prev = &raw mut (*cur).next;
            cur = (*cur).next;
        }

        let b = block as *mut Free;
        (*b).size = size;
        (*b).next = cur;
        *prev = b;

        // Forward: does this block run straight into the next one?
        if !cur.is_null() && block.add(size) == cur as *mut u8 {
            (*b).size += (*cur).size;
            (*b).next = (*cur).next;
        }
        // Backward: does the previous block run straight into this one? `prev`
        // points at a `next` field, and a `next` field is at a known offset
        // inside its own block -- except when it is the list head, which is
        // the case this check excludes.
        if prev != &raw mut h.free {
            let pblock = (prev as usize - core::mem::offset_of!(Free, next)) as *mut Free;
            if (pblock as *mut u8).add((*pblock).size) == block {
                (*pblock).size += (*b).size;
                (*pblock).next = (*b).next;
            }
        }
    }
}
