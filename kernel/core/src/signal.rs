//! Userspace signals: dispositions nk owns, delivery on the way back to EL0.
//!
//! Every signal number here is Linux's -- `SIGCHLD` is 17 because that is
//! what aarch64 binaries contain -- but the *behaviour* is nk's. LKL records
//! a disposition when a process calls `rt_sigaction` through `forward()`;
//! nothing would ever act on it, because EL0 never returns through Linux.
//! So the signal syscalls are intercepted in `rust_el0_sync` and answered
//! here, against per-task state in the scheduler.
//!
//! What is implemented: `rt_sigaction` (record/ignore/default), `rt_sigprocmask`
//! (block/unblock/inherit across fork), `kill`/`tkill`/`tgkill` (post to a
//! task by Linux pid), `sigaltstack` (recorded; delivery honours it),
//! `rt_sigpending` (what is pending and blocked), `rt_sigsuspend` and
//! `rt_sigtimedwait` (wait for a signal), `rt_sigreturn` (leave the handler),
//! and `SIGCHLD` on child exit. `SIGKILL` and `SIGSTOP` cannot be caught or
//! blocked -- the kernel enforces that, not the process.
//!
//! Delivery happens on the way back to EL0: after any syscall, and after an
//! interrupt, the return path checks for a pending unblocked signal with a
//! handler and, if there is one, builds the `rt_sigframe` on the user's
//! stack and redirects the frame to the handler. `rt_sigreturn` restores.
//!
//! What is not: `SA_SIGINFO` handlers receive a full `siginfo` and `ucontext`
//! (they do -- the frame is always the `rt_` shape); `signalfd`, `sigqueueinfo`
//! with data, `sigtimedwait` timeouts (returns EINTR when a signal arrives,
//! like everything else that waits here); stopping (`SIGSTOP`/`SIGCONT`
//! terminate rather than stop -- job control needs a process-group layer nk
//! does not have yet).

use crate::sched;
#[cfg(nk_lkl)]
use crate::uaccess;

/// Signal numbers Linux uses on every architecture that matters here.
/// The full table, even the ones nk never acts on: a signal API that cannot
/// name `SIGTERM` is not the API binaries were compiled against.
pub const SIGHUP: u64 = 1;
pub const SIGINT: u64 = 2;
pub const SIGQUIT: u64 = 3;
pub const SIGILL: u64 = 4;
pub const SIGTRAP: u64 = 5;
pub const SIGABRT: u64 = 6;
pub const SIGBUS: u64 = 7;
pub const SIGFPE: u64 = 8;
pub const SIGKILL: u64 = 9;
pub const SIGUSR1: u64 = 10;
pub const SIGSEGV: u64 = 11;
pub const SIGUSR2: u64 = 12;
pub const SIGPIPE: u64 = 13;
pub const SIGALRM: u64 = 14;
pub const SIGTERM: u64 = 15;
pub const SIGSTKFLT: u64 = 16;
pub const SIGCHLD: u64 = 17;
pub const SIGCONT: u64 = 18;
pub const SIGSTOP: u64 = 19;
pub const SIGTSTP: u64 = 20;
pub const SIGTTIN: u64 = 21;
pub const SIGTTOU: u64 = 22;
pub const SIGURG: u64 = 23;
pub const SIGXCPU: u64 = 24;
pub const SIGXFSZ: u64 = 25;
pub const SIGVTALRM: u64 = 26;
pub const SIGPROF: u64 = 27;
pub const SIGWINCH: u64 = 28;
pub const SIGIO: u64 = 29;
pub const SIGPWR: u64 = 30;
pub const SIGSYS: u64 = 31;

/// `SIG_DFL` (0) and `SIG_IGN` (1) are not addresses. Anything else is.
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// `sa_flags` bits the kernel honours here. `SA_SIGINFO` (4) selects the
/// three-argument handler, which is the only shape nk builds -- the frame
/// is always the `rt_` one, so every handler gets `siginfo` and `ucontext`
/// whether it asked or not.
#[allow(dead_code)]
pub const SA_SIGINFO: u64 = 4;
const SA_ONSTACK: u64 = 0x0800_0000;
#[allow(dead_code)]
pub const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
#[allow(dead_code)]
pub const SA_RESTORER: u64 = 0x0400_0000;

/// `sigprocmask` operations.
const SIG_BLOCK: u64 = 0;
const SIG_UNBLOCK: u64 = 1;
const SIG_SETMASK: u64 = 2;

/// What `rt_sigaction` stores. Two shapes cross this syscall and they must
/// not be confused.
///
/// The *kernel* `struct sigaction` (asm-generic `signal.h`) is handler at
/// 0, flags at 8, restorer at 16, mask at 24 -- 32 bytes plus a 128-byte
/// mask. The *glibc* `struct sigaction` (`bits/sigaction.h`) is handler at
/// 0, 128-byte mask at 8, flags at 136, restorer at 144 -- 152 bytes.
/// `rt_sigaction` receives the glibc shape (it is glibc that calls), so the
/// handler is read at 0, the first mask word at 8, flags at 136, restorer
/// at 144. The kernel shape matters nowhere here: LKL never sees these
/// calls, because they are intercepted before forwarding.
#[derive(Clone, Copy)]
pub struct Action {
    pub handler: u64,
    pub mask: u64,
    pub flags: u64,
    pub restorer: u64,
}

impl Action {
    pub const fn default() -> Action {
        Action {
            handler: SIG_DFL,
            mask: 0,
            flags: 0,
            restorer: 0,
        }
    }
}

/// Per-task signal state. `actions` is shared across threads (same address
/// space, same dispositions); `mask` and `pending` are per task. Stored in
/// the scheduler next to everything else that is per task.
#[derive(Clone, Copy)]
pub struct State {
    pub actions: [Action; 64],
    pub mask: u64,
    /// Signals that have arrived and not been delivered. Bit `n` is signal
    /// `n+1`. `SIGKILL`/`SIGSTOP` live here too: they are never blocked and
    /// have no handler, so delivery terminates/stops without consulting one.
    pub pending: u64,
    /// Alternate stack from `sigaltstack`: where handlers with `SA_ONSTACK`
    /// run. Zero when none is registered.
    pub alt_base: u64,
    pub alt_size: u64,
    pub alt_in_use: bool,
}

impl State {
    pub const fn new() -> State {
        State {
            actions: [Action::default(); 64],
            mask: 0,
            pending: 0,
            alt_base: 0,
            alt_size: 0,
            alt_in_use: false,
        }
    }

    /// A blank state for a module that cannot see one. Unused: the
    /// scheduler owns every `State` that matters.
    #[allow(dead_code)]
    pub fn blank() -> State {
        State::new()
    }
}

/// Whether a signal can be caught or blocked. The kernel owns these two.
fn uncatchable(sig: u64) -> bool {
    sig == SIGKILL || sig == SIGSTOP
}

/// Bit for signal `sig` (1-based) in a mask word.
fn bit(sig: u64) -> u64 {
    if sig == 0 || sig > 64 {
        0
    } else {
        1 << (sig - 1)
    }
}

/// # rt_sigaction(sig, action, oldaction)
///
/// Records the disposition. `SIGKILL`/`SIGSTOP` cannot be caught: asking is
/// `-EINVAL`, which is what Linux answers and what a libc expects when it
/// probes. A null `action` queries: the old disposition goes to `oldaction`
/// and nothing changes.
#[cfg(nk_lkl)]
pub fn sigaction(sig: u64, action: u64, old: u64) -> i64 {
    if sig == 0 || sig > 64 {
        return -22; // -EINVAL
    }
    if uncatchable(sig) && action != 0 {
        // Querying is fine; installing is not.
        let mut probe = [0u8; 8];
        if action != 0 && crate::uaccess::copy_from_user(&mut probe, action).is_err() {
            return crate::uaccess::EFAULT;
        }
        return -22;
    }
    let me = sched::current_id();
    if old != 0 {
        // `oact` names the same compact kernel layout glibc built for the
        // call: handler/flags/restorer/mask at 0/8/16/24, mask one word.
        let cur = sched::signal_action(me, sig);
        let mut buf = [0u8; 32];
        buf[0..8].copy_from_slice(&cur.handler.to_le_bytes());
        buf[8..16].copy_from_slice(&cur.flags.to_le_bytes());
        buf[16..24].copy_from_slice(&cur.restorer.to_le_bytes());
        buf[24..32].copy_from_slice(&cur.mask.to_le_bytes());
        if uaccess::copy_to_user(old, &buf).is_err() {
            return uaccess::EFAULT;
        }
    }
    if action != 0 {
        // Which layout crosses the syscall? Disassembled from glibc
        // (`__libc_sigaction`, Debian trixie arm64): the wrapper builds a
        // *compact kernel struct* on its own stack -- handler+flags at
        // sp+8, 128-bit mask at sp+32, restorer at sp+24 only when the
        // caller's SA_RESTORER is set -- and passes *that* to rt_sigaction
        // (134). So nk sees the kernel layout, not the 152-byte glibc
        // shape: handler at act+0, flags at act+8, restorer at act+16,
        // mask at act+24 (first word; the mask is 128 bytes but nk tracks
        // 64 signals in one word). The earlier "glibc layout" reading was
        // wrong: it mistook the mask's zeros for flags and the restorer
        // slot for a stack word, which is why every install stored
        // handler-correct/flags-0/restorer-0 and every handler died on
        // return with x30 unset.
        let mut handler_b = [0u8; 8];
        let mut flags_b = [0u8; 8];
        let mut restorer_b = [0u8; 8];
        let mut mask_b = [0u8; 8];
        if uaccess::copy_from_user(&mut handler_b, action).is_err()
            || uaccess::copy_from_user(&mut flags_b, action + 8).is_err()
            || uaccess::copy_from_user(&mut restorer_b, action + 16).is_err()
            || uaccess::copy_from_user(&mut mask_b, action + 24).is_err()
        {
            return uaccess::EFAULT;
        }
        let handler = u64::from_le_bytes(handler_b);
        let mask = u64::from_le_bytes(mask_b);
        let flags = u64::from_le_bytes(flags_b);
        let restorer = u64::from_le_bytes(restorer_b);
        // Honour the flag exactly as sent. glibc on aarch64 NEVER fills
        // `sa_restorer` and NEVER sets `SA_RESTORER`: the wrapper builds
        // the compact kernel struct on its own stack, and the restorer
        // slot arrives holding whatever was on that stack (measured
        // values like 0x400bc0 and 0x2efe15f8 were noise, not stubs). An
        // earlier version synthesised the flag from `restorer != 0`,
        // which trusted that garbage and sent returning handlers into
        // unmapped text.
        sched::set_signal_action(me, sig, Action {
            handler,
            mask,
            flags,
            restorer,
        });
        // A handler installed for a pending signal makes it deliverable:
        // wake the task in case it is waiting in `sigsuspend` or `pause`.
        if handler != SIG_DFL && handler != SIG_IGN {
            sched::wake(me);
        }
    }
    0
}

/// # rt_sigprocmask(how, set, oldset)
///
/// `SIGKILL`/`SIGSTOP` cannot be blocked: the bits are silently dropped,
/// which is Linux's behaviour and what a libc relies on when it blocks
/// "everything".
#[cfg(nk_lkl)]
pub fn sigprocmask(how: u64, set: u64, old: u64) -> i64 {
    let me = sched::current_id();
    if old != 0 {
        let mask = sched::signal_mask(me).to_le_bytes();
        if uaccess::copy_to_user(old, &mask).is_err() {
            return uaccess::EFAULT;
        }
    }
    if set != 0 {
        let mut buf = [0u8; 8];
        if uaccess::copy_from_user(&mut buf, set).is_err() {
            return uaccess::EFAULT;
        }
        let mut bits = u64::from_le_bytes(buf);
        bits &= !(bit(SIGKILL) | bit(SIGSTOP));
        // `SIGSTOP`/`SIGKILL` bits dropped above; realtime half (33-64)
        // accepted and stored -- nk delivers those like the rest.
        match how {
            SIG_BLOCK => sched::set_signal_mask(me, sched::signal_mask(me) | bits),
            SIG_UNBLOCK => sched::set_signal_mask(me, sched::signal_mask(me) & !bits),
            SIG_SETMASK => sched::set_signal_mask(me, bits),
            _ => return -22,
        }
    }
    0
}

/// # rt_sigpending(set)
///
/// What is pending, whether blocked or not. glibc uses this to poll for a
/// signal it blocked and intends to handle synchronously.
#[cfg(nk_lkl)]
pub fn sigpending(set: u64) -> i64 {
    if set == 0 {
        return 0;
    }
    let pending = sched::signal_pending(sched::current_id()).to_le_bytes();
    if uaccess::copy_to_user(set, &pending).is_err() {
        return uaccess::EFAULT;
    }
    0
}

/// Post `sig` to the task with Linux pid `pid`. Used by `kill`, `tkill`,
/// `tgkill` and by `sys_exit` for `SIGCHLD`. Returns 0, `-ESRCH`, or
/// `-EINVAL` for a bad number.
#[cfg(nk_lkl)]
pub fn post(pid: i64, sig: u64) -> i64 {
    if sig > 64 {
        return -22; // -EINVAL
    }
    if sig == 0 {
        // Signal 0 is existence: no delivery, just "is there such a task".
        return if sched::task_with_linux_pid(pid).is_some() {
            0
        } else {
            -3 // -ESRCH
        };
    }
    let Some(id) = sched::task_with_linux_pid(pid) else {
        return -3;
    };
    sched::add_signal_pending(id, bit(sig));
    // A pending unblocked signal must interrupt a wait: `futex`, `wait4`,
    // `sigsuspend` all check pending on wake and return `-EINTR`.
    sched::wake(id);
    0
}

/// # kill(pid, sig)
///
/// `pid > 0` posts to that task; `pid == 0` posts to every task in the
/// caller's group (every task with the same parent -- nk has no sessions);
/// `pid == -1` posts to every task but the caller. Negative `pid < -1`
/// (process groups) is refused: nk has no groups yet.
#[cfg(nk_lkl)]
pub fn kill(pid: i64, sig: u64) -> i64 {
    if sig > 64 {
        return -22;
    }
    if pid > 0 {
        return post(pid, sig);
    }
    if pid == 0 || pid == -1 {
        let me = sched::current_id();
        let mine = sched::linux_pid(me);
        let parent = sched::task_parent(me);
        let mut found = false;
        for id in 0..sched::MAX_TASKS {
            if id == me && pid == -1 {
                continue;
            }
            let (lpid, lparent, live) = sched::task_identity(id);
            if !live || lpid <= 1 {
                continue;
            }
            let same = if pid == 0 {
                lparent == parent || lpid == mine
            } else {
                true
            };
            if same {
                if post(lpid, sig) == 0 {
                    found = true;
                }
            }
        }
        return if found { 0 } else { -3 };
    }
    -22 // process groups: nk has no groups yet
}

/// Deliver to the running task what is pending, unblocked, and handled.
///
/// Called on the way back to EL0 -- after every syscall and after every
/// timer interrupt -- so a signal arrives promptly without preempting the
/// kernel mid-syscall. At most one signal per return: the handler runs, it
/// calls `rt_sigreturn`, and the next return delivers the next.
///
/// Returns true when the frame was redirected: the caller must not overwrite
/// `x0` with the syscall return.
#[cfg(nk_lkl)]
pub fn deliver(frame: &mut crate::user::Frame) -> bool {
    let me = sched::current_id();
    let (pending, mask) = (sched::signal_pending(me), sched::signal_mask(me));
    let mut cand = pending & !mask;
    // Unblocked even when masked: SIGKILL/SIGSTOP answer to nothing.
    cand |= pending & (bit(SIGKILL) | bit(SIGSTOP));
    if cand == 0 {
        return false;
    }
    // Lowest number first, like Linux.
    let sig = cand.trailing_zeros() as u64 + 1;
    if uncatchable(sig) {
        // `SIGKILL` terminates, `SIGTERM`-by-default terminates; `SIGSTOP`
        // would stop, and without job control stopping is terminating.
        // `SIGCONT` is a no-op delivered and forgotten.
        sched::clear_signal_pending(me, bit(sig));
        if sig == SIGCONT {
            return false;
        }
        crate::user::sys_exit(-(sig as i32));
    }
    let act = sched::signal_action(me, sig);
    if act.handler == SIG_DFL {
        // Default dispositions: the ones a shell cannot survive without.
        // `SIGCHLD`/`SIGCONT`/`SIGURG` are ignored; everything else with no
        // handler terminates. `SIGSTOP` is handled above.
        sched::clear_signal_pending(me, bit(sig));
        match sig {
            SIGCHLD | SIGCONT | SIGURG => return false,
            _ => crate::user::sys_exit(-(sig as i32)),
        }
    }
    if act.handler == SIG_IGN {
        sched::clear_signal_pending(me, bit(sig));
        return false;
    }
    // A handler. Block the signal itself unless `SA_NODEFER`, plus the
    // handler's mask; unblock on `rt_sigreturn`.
    sched::clear_signal_pending(me, bit(sig));
    let mut mask = sched::signal_mask(me) | act.mask;
    if act.flags & SA_NODEFER == 0 {
        mask |= bit(sig);
    }
    sched::set_signal_mask(me, mask);
    build_frame(frame, me, sig, &act);
    true
}

/// Build the `rt_sigframe` on the user's stack and redirect the frame.
///
/// The stack, honouring the alternate stack: with `SA_ONSTACK` and a
/// registered altstack that is not already in use, the frame goes there;
/// otherwise it goes on the interrupted stack, 16-byte aligned, below the
/// current pointer. The frame's `elr` becomes the handler, `x0` the signal
/// number, `x1` a pointer to `siginfo`, `x2` a pointer to `ucontext`.
/// `rt_sigreturn` (139) restores everything from the `ucontext`.
#[cfg(nk_lkl)]
fn build_frame(frame: &mut crate::user::Frame, me: usize, sig: u64, act: &Action) {
    // 128 (siginfo) + 4560 (ucontext) + 128 (restorer gap), rounded to
    // keep the stack 16-byte aligned: 128+4560 = 4688, +16 = 4704.
    const FRAME_SIZE: u64 = 4704;
    let sp = if act.flags & SA_ONSTACK != 0 {
        let (base, size, in_use) = sched::signal_altstack(me);
        if base != 0 && !in_use {
            sched::set_signal_altstack_in_use(me, true);
            base + size - FRAME_SIZE
        } else {
            frame.sp - FRAME_SIZE
        }
    } else {
        frame.sp - FRAME_SIZE
    };
    let sp = sp & !15;
    // siginfo at sp: signo at 0, errno at 4, code at 8, pid at 16, uid
    // at 20 -- measured against Debian trixie's headers, not assumed.
    // `SI_USER` (0) for a `kill`, `SI_KERNEL` (128) for a `SIGCHLD` from
    // `sys_exit` -- recorded per signal; the sender's pid and a zero uid
    // go in the standard slots.
    let mut info = [0u8; 128];
    info[0..4].copy_from_slice(&(sig as i32).to_le_bytes());
    info[4..8].copy_from_slice(&0i32.to_le_bytes());
    info[8..12].copy_from_slice(&(sched::signal_code(me, sig) as i32).to_le_bytes());
    let sender = sched::signal_sender(me, sig);
    info[16..20].copy_from_slice(&sender.to_le_bytes());
    info[20..24].copy_from_slice(&0u32.to_le_bytes());
    // ucontext at sp+128: flags, null link, current stack descriptor,
    // current mask, then the mcontext: fault address zero, the interrupted
    // registers, sp, pc, pstate, and a zeroed `__reserved` (no FPSIMD
    // state -- nk never enables EL0 floating point, and a handler that
    // touches vector registers faults, which is honest until it is not).
    let mut uc = [0u8; 4560];
    // uc_flags 0; uc_link NULL; uc_stack: current sp, no flags.
    uc[16..24].copy_from_slice(&frame.sp.to_le_bytes());
    uc[24..32].copy_from_slice(&0u64.to_le_bytes());
    uc[32..40].copy_from_slice(&0u64.to_le_bytes());
    let mask = sched::signal_mask(me);
    uc[40..48].copy_from_slice(&mask.to_le_bytes());
    // mcontext at uc+176: fault 0, regs[31] = frame x, sp, pc, pstate.
    let m = 176;
    for i in 0..31 {
        uc[m + 8 + i * 8..m + 16 + i * 8].copy_from_slice(&frame.x[i].to_le_bytes());
    }
    uc[m + 256..m + 264].copy_from_slice(&frame.sp.to_le_bytes());
    uc[m + 264..m + 272].copy_from_slice(&frame.elr.to_le_bytes());
    uc[m + 272..m + 280].copy_from_slice(&frame.spsr.to_le_bytes());
    // The frame's own address, so `sigreturn` finds it without trusting
    // any register: the handler may clobber `x2` (caller-saved, held the
    // ucontext pointer on entry) and move `sp` (any call spills). Linux
    // hides a cookie in `uc_flags` (`0x5050534f`, "SOSP"); nk writes the
    // ucontext's own address at uc_flags instead -- same hiding place,
    // exact rather than probabilistic. `sigreturn` reads it back and
    // validates it points at readable memory before restoring.
    uc[0..8].copy_from_slice(&(sp + 128).to_le_bytes());
    // __reserved (m+288, 4096 bytes) stays zero: no extra context.
    let saved_mask = sched::signal_mask(me);
    if uaccess::copy_to_user(sp, &info).is_err() || uaccess::copy_to_user(sp + 128, &uc).is_err() {
        // The stack cannot hold the frame: the process is out of address
        // space or its pointer is corrupt. Terminate rather than deliver
        // half a handler, which would run with a garbage ucontext.
        crate::user::sys_exit(-(sig as i32));
    }
    // Stash what `rt_sigreturn` needs where only the kernel reads it: the
    // saved mask and whether the altstack was entered. The ucontext on the
    // user stack carries the registers; the kernel carries the rest.
    sched::set_signal_return(me, saved_mask, act.flags & SA_ONSTACK != 0);
    frame.sp = sp;
    frame.elr = act.handler;
    // x30 is the return address, per arm64 `setup_return()`: the restorer
    // the caller passed when `SA_RESTORER` is set, else nk's own
    // trampoline -- one page per address space, executable and read-only
    // (Linux uses its VDSO sigtramp; nk has no VDSO). A returning handler
    // lands on `rt_sigreturn` (139) either way.
    frame.x[30] = if act.flags & SA_RESTORER != 0 {
        act.restorer
    } else {
        match trampoline_addr() {
            Some(t) => t,
            // Genuinely missing trampoline: a mapping failure, not a
            // normal path. Poison loudly rather than falling through.
            None => 0xDEAD_0000_0000_0000,
        }
    };
    frame.x[0] = sig;
    frame.x[1] = sp;
    frame.x[2] = sp + 128;
}

/// # rt_sigreturn()
///
/// Leave the handler: restore the interrupted registers, stack pointer and
/// mask from the `ucontext` the frame was built with.
///
/// The ucontext is found via the self-pointer `build_frame` hid in
/// `uc_flags` (the frame's own address), not via `x2` and not via live
/// `sp`: `x2` carried the pointer into the handler but is caller-saved,
/// and `sp` moves the moment the handler calls anything. Both were tried;
/// both fail on any non-trivial handler (busybox `sh` mounting a disk was
/// the one that proved it: `sp` had moved to `0x2fffe260`, `x2` held
/// whatever the handler left, and the restore read garbage). The
/// self-pointer survives either clobber. It is validated -- 16-byte
/// aligned, below `USER_STACK_TOP`, readable -- before use; anything else
/// is `-EFAULT`.
#[cfg(nk_lkl)]
pub fn sigreturn(frame: &mut crate::user::Frame) -> i64 {
    // Two candidate sources: live sp+128 (handler kept the stack) and x2
    // (handler moved sp but preserved the pointer, e.g. a leaf). Each
    // candidate is validated by its self-pointer -- uc_flags must equal
    // the base it was read from -- before the 4560-byte read, so a wrong
    // guess costs one word probe, not a garbage restore.
    let mut uc_at = 0u64;
    for base in [frame.sp.checked_add(128).unwrap_or(0), frame.x[2]] {
        if base == 0 || base & 15 != 0 || base >= crate::user::USER_STACK_TOP {
            continue;
        }
        let mut flag_b = [0u8; 8];
        if crate::uaccess::copy_from_user(&mut flag_b, base).is_err() {
            continue;
        }
        if u64::from_le_bytes(flag_b) == base {
            uc_at = base;
            break;
        }
    }
    if uc_at == 0 {
        return -14; // -EFAULT: no recognisable frame at either source
    }
    let mut uc = [0u8; 4560];
    // Read the whole ucontext first: a half-restored frame is worse than
    // none, and the copy either arrives whole or not at all.
    if crate::uaccess::copy_from_user(&mut uc, uc_at).is_err() {
        return -14;
    }
    let m = 176;
    for i in 0..31 {
        frame.x[i] = u64::from_le_bytes(uc[m + 8 + i * 8..m + 16 + i * 8].try_into().unwrap());
    }
    frame.sp = u64::from_le_bytes(uc[m + 256..m + 264].try_into().unwrap());
    frame.elr = u64::from_le_bytes(uc[m + 264..m + 272].try_into().unwrap());
    frame.spsr = u64::from_le_bytes(uc[m + 272..m + 280].try_into().unwrap());
    let (saved_mask, used_alt) = sched::signal_return(sched::current_id());
    sched::set_signal_mask(sched::current_id(), saved_mask);
    if used_alt {
        sched::set_signal_altstack_in_use(sched::current_id(), false);
    }
    // The return value is the restored `x0`, not a fresh one: `deliver`
    // must not overwrite it. Signalled by returning a sentinel the
    // dispatcher recognises -- see `rust_el0_sync`.
    -512
}

/// nk's signal trampoline: one page per address space, mapped
/// executable and read-only at a fixed address, holding
/// `mov x8, #139; svc #0` -- `rt_sigreturn` -- so a handler that returns
/// lands back in the kernel the way it does off Linux's VDSO sigtramp.
/// d2801168 little-endian is `68 11 80 d2`; d4000001 is `01 00 00 d4`.
///
/// The address sits above `USER_STACK_TOP`, where no user syscall can
/// reach it: `mmap`/`brk` are capped at `USER_MMAP_TOP`, and
/// `munmap`/`mprotect` refuse anything at or above `USER_STACK_TOP`.
/// `fork` copies it like any other page (permissions preserved) and
/// process teardown frees it.
#[cfg(nk_lkl)]
const TRAMPOLINE_ADDR: u64 = 0x3FF0_0000;
#[cfg(nk_lkl)]
const TRAMPOLINE_CODE: [u8; 8] = [0x68, 0x11, 0x80, 0xd2, 0x01, 0x00, 0x00, 0xd4];

/// The address `build_frame` puts in x30 when `SA_RESTORER` is clear.
/// Maps the trampoline into this address space on first use. Runs on the
/// task's own return path, so the current tables are the task's.
#[cfg(nk_lkl)]
fn trampoline_addr() -> Option<u64> {
    if crate::paging::user_to_phys(TRAMPOLINE_ADDR).is_some() {
        return Some(TRAMPOLINE_ADDR);
    }
    let page = crate::frames::alloc()? as u64;
    unsafe {
        core::ptr::copy_nonoverlapping(TRAMPOLINE_CODE.as_ptr(), page as *mut u8, TRAMPOLINE_CODE.len());
        let mut cur: u64;
        core::arch::asm!("mrs {}, ttbr0_el1", out(reg) cur, options(nomem, nostack));
        cur = crate::paging::table_of(cur);
        // Executable and read-only: `map_user_permissions` cleans the
        // icache for the freshly written bytes on the mapping path.
        crate::paging::map_user_permissions(cur, TRAMPOLINE_ADDR, page, 4096, true, false);
    }
    Some(TRAMPOLINE_ADDR)
}
/// nk -- `futex`, `wait4`, `sigsuspend` -- asks this after waking: a
/// pending unblocked signal means `-EINTR`, not the waited-for event.
#[cfg(nk_lkl)]
pub fn interrupt_pending() -> bool {
    let me = sched::current_id();
    let cand = sched::signal_pending(me) & !sched::signal_mask(me);
    cand & !(bit(SIGKILL) | bit(SIGSTOP)) != 0 || cand & (bit(SIGKILL) | bit(SIGSTOP)) != 0
}

/// The signal to report for a wait interrupted by delivery. Lowest pending
/// unblocked number, like `deliver`.
#[cfg(nk_lkl)]
pub fn interrupt_signal() -> u64 {
    let me = sched::current_id();
    let cand = sched::signal_pending(me) & !sched::signal_mask(me)
        | sched::signal_pending(me) & (bit(SIGKILL) | bit(SIGSTOP));
    if cand == 0 {
        return 0;
    }
    cand.trailing_zeros() as u64 + 1
}
