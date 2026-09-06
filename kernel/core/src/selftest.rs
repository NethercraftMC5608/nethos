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

extern "C" {
    fn nk_setjmp(buf: *mut u64) -> i32;
    fn nk_longjmp(buf: *mut u64, val: i32) -> !;
}

/// 128 words, matching LKL's `struct lkl_jmp_buf`.
static mut JB: [u64; 128] = [0; 128];
static mut JUMP_WITH: i32 = 0;

#[inline(never)]
extern "C" fn jump_back() {
    unsafe { nk_longjmp(&raw mut JB as *mut u64, JUMP_WITH) }
}

fn setjmp_roundtrip(val: i32) -> i32 {
    unsafe {
        JUMP_WITH = val;
        let r = nk_setjmp(&raw mut JB as *mut u64);
        if r == 0 {
            jump_back();
        }
        r
    }
}

/// The property that actually matters: a value the compiler decided to keep
/// in a callee-saved register across the `setjmp` call must still be there
/// after the `longjmp`. That is the entire contract, and it is what a wrong
/// register list breaks silently.
#[inline(never)]
fn callee_saved_survive() -> bool {
    unsafe {
        JUMP_WITH = 7;
        // Enough live values that some must land in x19-x28.
        let (a, b, c, d) = (0x1111u64, 0x2222u64, 0x3333u64, 0x4444u64);
        let r = nk_setjmp(&raw mut JB as *mut u64);
        if r == 0 {
            jump_back();
        }
        core::hint::black_box(a) == 0x1111
            && core::hint::black_box(b) == 0x2222
            && core::hint::black_box(c) == 0x3333
            && core::hint::black_box(d) == 0x4444
    }
}

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

    // setjmp and longjmp.
    //
    // Written from scratch for LKL, which uses them to hand the CPU between
    // host threads, and never exercised until now -- nk's own code has no
    // reason to jump out of a call. Untested assembly that saves the wrong
    // registers does not fail where it is written; it fails much later, in
    // whatever was relying on a callee-saved register surviving.
    {
        check!(setjmp_roundtrip(42) == 42, "longjmp did not carry its value");
        // longjmp(buf, 0) must still look like a non-zero return, or the
        // caller cannot tell the two paths apart. C requires it and it is the
        // easiest half of the contract to leave out.
        check!(setjmp_roundtrip(0) == 1, "longjmp(buf, 0) did not return 1");
        check!(callee_saved_survive(), "a callee-saved register did not survive longjmp");
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
        println!("  check:  heap ok, setjmp/longjmp ok");
    } else {
        panic!("{} self-test failures", failures);
    }
}
