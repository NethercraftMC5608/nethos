//! Checks nk runs on itself at boot.
//!
//! A kernel has no test harness, no process to run one in, and nothing to
//! report a failure to. What it does have is the ability to exercise its own
//! allocator before anything depends on it and say plainly whether it worked
//! -- which is worth far more here than it would be in userspace, because the
//! symptom of a subtly wrong allocator is a driver misbehaving three stages
//! later.
//!
//! Cheap enough to leave on: a few milliseconds, once, at boot.

use crate::heap;
use crate::println;
use alloc::boxed::Box;
use alloc::vec::Vec;

pub fn run() {
    let (used_before, _) = heap::stats();
    let mut failures = 0;

    macro_rules! check {
        ($cond:expr, $($msg:tt)*) => {
            if !($cond) {
                failures += 1;
                println!("  FAIL  {}", format_args!($($msg)*));
            }
        };
    }

    // A box, and the pointer it hands back.
    {
        let b = Box::new(0x1234_5678_9abc_def0u64);
        check!(*b == 0x1234_5678_9abc_def0, "box did not hold its value");
    }

    // A vector that outgrows its allocation repeatedly, which is the path that
    // exercises realloc: allocate, copy, free the old block.
    {
        let mut v: Vec<usize> = Vec::new();
        for i in 0..10_000 {
            v.push(i * 3);
        }
        check!(v.len() == 10_000, "vec lost elements");
        check!(v[9_999] == 29_997, "vec corrupted its contents");
    }

    // Alignment. Nothing in Rust asks for 4096 by itself, but Linux drivers
    // ask for page-aligned DMA buffers constantly, and the alignment padding
    // path is the one place the header can end up somewhere it should not.
    {
        let layout = core::alloc::Layout::from_size_align(64, 4096).unwrap();
        unsafe {
            let p = alloc::alloc::alloc(layout);
            check!(!p.is_null(), "4096-aligned allocation failed");
            check!(p as usize % 4096 == 0, "allocation was not 4096-aligned: {:#x}", p as usize);
            alloc::alloc::dealloc(p, layout);
        }
    }

    // Fragment the arena deliberately, then put it back. Freeing in a
    // different order from allocation is the case first fit gets wrong, and
    // the interleaved free is what forces coalescing to work in both
    // directions rather than only forwards.
    {
        let mut blocks: Vec<Vec<u8>> = Vec::new();
        for i in 0..64 {
            blocks.push(alloc::vec![i as u8; 128 + i * 16]);
        }
        for (i, b) in blocks.iter().enumerate() {
            check!(b[0] == i as u8, "block {} was overwritten", i);
        }
        let mut odds: Vec<Vec<u8>> = Vec::new();
        let mut evens: Vec<Vec<u8>> = Vec::new();
        for (i, b) in blocks.into_iter().enumerate() {
            if i % 2 == 0 { evens.push(b) } else { odds.push(b) }
        }
        drop(odds);
        drop(evens);
    }

    let (used_after, _) = heap::stats();
    check!(
        used_after == used_before,
        "heap leaked: {} bytes still used, was {}",
        used_after,
        used_before
    );
    let blocks = heap::free_blocks();
    check!(blocks == 1, "heap did not coalesce: {} free blocks, expected 1", blocks);

    if failures == 0 {
        println!("  check:  heap ok (box, vec, realloc, 4K alignment, coalescing)");
    } else {
        panic!("{} self-test failures", failures);
    }
}
