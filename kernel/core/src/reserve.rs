//! Address space a process has asked for but not yet touched.
//!
//! nk populates a mapping when it is made: a frame per page, read or zeroed,
//! whether or not the program ever looks at it. That is the simplest thing
//! that works and it is what `MAP_NORESERVE` exists to say is wrong.
//!
//! WebKit is the first program here to care. Its allocator reserves address
//! space in very large blocks -- 128MB at a time -- and commits a fraction of
//! it. Populating those eagerly costs a frame per page of a region nothing
//! reads, and the arena runs out long before the machine does: the failure
//! is `mmapfail range len 0x8000000` with a gigabyte already spoken for.
//!
//! So a `MAP_NORESERVE` anonymous mapping is recorded here and mapped
//! nowhere. The first touch of each page faults, and `fault_in` puts a zeroed
//! frame under it. That is demand paging, in the one case that needs it,
//! rather than a page-cache rewrite.
//!
//! Deliberately not general. File-backed mappings are still eager (they have
//! contents to fetch and no flag asking otherwise), and a reservation that is
//! never touched costs one table entry here and nothing else.

use crate::frames;
use crate::paging;

const MAX: usize = 64;

#[derive(Clone, Copy)]
struct Reservation {
    /// The address space this belongs to, so threads sharing one see the
    /// same reservations and two processes at the same address do not.
    space: u64,
    start: u64,
    end: u64,
    writable: bool,
    exec: bool,
    live: bool,
}

static mut RESERVED: [Reservation; MAX] = [Reservation {
    space: 0,
    start: 0,
    end: 0,
    writable: false,
    exec: false,
    live: false,
}; MAX];

fn space() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) v, options(nomem, nostack)) };
    paging::table_of(v)
}

/// Record a range as reserved but unmapped. False when the table is full,
/// which the caller must treat as a refusal rather than ignore: a
/// reservation nobody recorded is a segfault on first touch.
pub fn add(start: u64, len: u64, writable: bool, exec: bool) -> bool {
    let here = space();
    unsafe {
        let r = &mut *(&raw mut RESERVED);
        if let Some(slot) = r.iter_mut().find(|s| !s.live) {
            *slot = Reservation {
                space: here,
                start,
                end: start + len,
                writable,
                exec,
                live: true,
            };
            return true;
        }
    }
    false
}

/// Forget any reservation overlapping this range. Pages already faulted in
/// are ordinary mappings and are freed by the caller's own unmap.
pub fn forget(start: u64, len: u64) {
    let here = space();
    let end = start + len;
    unsafe {
        for s in (*(&raw mut RESERVED)).iter_mut() {
            if s.live && s.space == here && s.start < end && start < s.end {
                s.live = false;
            }
        }
    }
}

/// Give a new address space the reservations of the one it was copied from.
///
/// `fork` copies pages, and a reservation has none: it is a promise that a
/// range will get one on first touch. Without carrying the promise across,
/// the child faults on the first byte of an arena its parent could use.
pub fn inherit(parent: u64, child: u64) {
    unsafe {
        let r = &mut *(&raw mut RESERVED);
        for i in 0..MAX {
            if !(r[i].live && r[i].space == parent) {
                continue;
            }
            let mut copy = r[i];
            copy.space = child;
            match r.iter_mut().find(|s| !s.live) {
                Some(slot) => *slot = copy,
                // Out of slots: the child simply has one fewer reservation
                // than its parent, and faults there as it would have before
                // MAP_NORESERVE was honoured at all.
                None => return,
            }
        }
    }
}

/// Drop every reservation belonging to an address space that is going away.
pub fn forget_space(table: u64) {
    unsafe {
        for s in (*(&raw mut RESERVED)).iter_mut() {
            if s.live && s.space == table {
                s.live = false;
            }
        }
    }
}

/// Put a page under `addr` if it falls in a reservation.
///
/// Called from the EL0 fault handler before it decides the process is at
/// fault. Returns whether the fault was handled, in which case the
/// instruction is retried and the program never learns anything happened.
pub fn fault_in(addr: u64) -> bool {
    let here = space();
    let page = addr & !4095;
    let (writable, exec) = unsafe {
        match (*(&raw const RESERVED))
            .iter()
            .find(|s| s.live && s.space == here && page >= s.start && page < s.end)
        {
            Some(s) => (s.writable, s.exec),
            None => return false,
        }
    };
    // alloc() zeroes, which is what an anonymous page must read as. Out of
    // memory here is a real refusal: say no rather than map something else.
    let Some(frame) = frames::alloc() else {
        return false;
    };
    unsafe {
        let root: u64;
        core::arch::asm!("mrs {}, ttbr0_el1", out(reg) root, options(nomem, nostack));
        paging::map_user_permissions(paging::table_of(root), page, frame as u64, 4096, exec, writable);
    }
    true
}
