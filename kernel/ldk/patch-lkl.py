#!/usr/bin/env python3
"""What nk needs changed in arch/lkl, and why.

Run inside ldk's container with the LKL tree as the working directory. Every
patch is idempotent, because the tree is a docker volume that survives between
builds and this runs on every one of them.

These are all the same kind of change: `arch/lkl` was written for a host that
is a Unix process, and nk is a host that is a kernel with hardware. Where the
two disagree the fix belongs here, in the architecture port, and not in a
translation layer on nk's side -- a translation layer is a table with an entry
per system call, which is the thing this project exists to avoid writing.
"""
import os
import shutil
import sys

# Four gigabytes of virtual address space for Linux's vmalloc arena, starting
# at four. nk's paging reserves the same window; the two have to agree.
VMALLOC_BASE = '0x100000000UL'
VMALLOC_TOP = '0x17fffffffUL'
# Linux's linear map at six gigabytes and its user mmap base at seven, both
# inside the window nk reserves and clear of its vmalloc arena.
# The linear map is a real physical address: nk reserves this range and hands
# it back from shmem_init, so Linux's identity __pa() tells the truth.
MEMORY_START = '0x50000000'
# The stack top, which arch/lkl otherwise puts just below the linear map --
# and therefore inside nk's identity map of RAM.
STACK_TOP = '0x1e0000000UL'
TASK_BASE = '0x1c0000000'

UACCESS_H = '''/* SPDX-License-Identifier: GPL-2.0 */
/*
 * User access for a host that has a real user address space.
 *
 * The stock arch/lkl assumes kernel and user memory are the same memory and
 * makes copy_from_user a memcpy. That is true of every host LKL was written
 * for and false of a host that runs its processes at EL0 with their own
 * translation tables. The host provides the three primitives below; every
 * other user accessor in the kernel is built out of them.
 */
#ifndef _ASM_LKL_UACCESS_H
#define _ASM_LKL_UACCESS_H

#include <linux/string.h>
#include <asm-generic/access_ok.h>

/* All three return the number of bytes NOT transferred, as Linux expects. */
unsigned long lkl_copy_from_user(void *to, const void *from, unsigned long n);
unsigned long lkl_copy_to_user(void *to, const void *from, unsigned long n);
unsigned long lkl_clear_user(void *to, unsigned long n);

static inline unsigned long
raw_copy_from_user(void *to, const void __user *from, unsigned long n)
{
\treturn lkl_copy_from_user(to, (__force const void *)from, n);
}
#define raw_copy_from_user raw_copy_from_user

static inline unsigned long
raw_copy_to_user(void __user *to, const void *from, unsigned long n)
{
\treturn lkl_copy_to_user((__force void *)to, from, n);
}
#define raw_copy_to_user raw_copy_to_user

static inline unsigned long __clear_user(void __user *to, unsigned long n)
{
\treturn lkl_clear_user((__force void *)to, n);
}
#define __clear_user __clear_user

#include <asm-generic/uaccess.h>

#endif /* _ASM_LKL_UACCESS_H */
'''


def say(what):
    print(f"  patched: {what}")


def edit(path, old, new, marker):
    """Replace `old` with `new` in `path`, once, unless `marker` is present."""
    s = open(path).read()
    if marker in s:
        return False
    if old not in s:
        sys.exit(f"patch-lkl: {path} no longer contains:\n{old}")
    open(path, 'w').write(s.replace(old, new, 1))
    return True


def arm64_open_flags():
    """arm64's open flags, not asm-generic's.

    arm64 overrides four of them, and the collision is the worst kind:
    O_DIRECTORY is 1<<14 on arm64 and 1<<14 is O_DIRECT in asm-generic. So a
    program opening a directory asks LKL for direct I/O, gets EINVAL from a
    filesystem that has none, and busybox reports "can't open '/': Invalid
    argument" with nothing anywhere mentioning a flag.

    Only the file arm64 actually overrides. Its other uapi/asm headers describe
    structures LKL defines for itself -- ptrace, sigcontext -- and copying those
    would replace working definitions with ones for hardware LKL does not have.
    """
    dst = 'arch/lkl/include/uapi/asm/fcntl.h'
    if os.path.exists(dst):
        return
    shutil.copy('arch/arm64/include/uapi/asm/fcntl.h', dst)
    # The generated wrapper around asm-generic is only created when there is no
    # real header, and it is already there from the last build.
    gen = 'arch/lkl/include/generated/uapi/asm/fcntl.h'
    if os.path.exists(gen):
        os.remove(gen)
    say("arch/lkl uses arm64's open flags")


def host_user_access():
    """Let Linux do its own user access.

    arch/lkl selects UACCESS_MEMCPY: kernel and user share one flat address
    space, so copy_from_user is a memcpy. On nk they do not share one, so
    every pointer-bearing syscall had to be described in a table on nk's side
    and its buffers bounced across.

    Linux already knows which arguments are user pointers -- it marks them
    __user -- so asking the host makes every system call work for the same
    reason it works on real hardware, and there is nothing left to enumerate.
    """
    dst = 'arch/lkl/include/asm/uaccess.h'
    if os.path.exists(dst):
        return
    edit('arch/lkl/Kconfig',
         '\tselect UACCESS_MEMCPY\n',
         '\t# UACCESS_MEMCPY dropped: see arch/lkl/include/asm/uaccess.h\n',
         'UACCESS_MEMCPY dropped')
    open(dst, 'w').write(UACCESS_H)
    say('arch/lkl asks the host for user access')


def host0_signals():
    """host0 must not become permanently unable to create tasks.

    LKL makes every host task with kernel_thread(), and switches to host0 --
    its own init -- to do it. kernel_thread() returns -ERESTARTNOINTR when a
    signal is pending on the caller, which is right for a task that will handle
    the signal and retry. host0 never handles anything: it is a kernel thread
    with no signal handling at all, so one pending signal is pending for ever
    and no host task can be created again. A shell that forks a few times meets
    this and reports "can't fork: Unknown error 513".

    Dropping them is safe for the reason it is necessary: nothing reads them.
    This is where LKL's task model shows through -- a real fork would clone the
    caller, and then the caller's signal state would be the one that mattered.
    """
    if edit('arch/lkl/kernel/syscalls.c',
            '\tswitch_to_host_task(host0);\n',
            '\tswitch_to_host_task(host0);\n'
            '\t/* nk: host0 never dequeues a signal, and one pending would\n'
            '\t * stop kernel_thread() making any further host task. */\n'
            '\tflush_signals(current);\n',
            'flush_signals'):
        say('host0 drops pending signals before creating a task')


def linux_address_space():
    """Put Linux's virtual addresses somewhere nk is not.

    With CONFIG_MMU, arch/lkl wants an address space to itself: its vmalloc
    arena is VMALLOC_START..VMALLOC_END, which it declares as 0..0xffffffff,
    and its linear map starts at CONFIG_LKL_MEMORY_START, which defaults to
    0x50000000. On every other LKL host that is a Unix process's address space
    and the low four gigabytes are free. On nk they are not: nk's image, the
    devices and the identity map of RAM are all in them, and Linux mapping
    over them would be a kernel overwriting itself.

    So Linux is moved above all of it -- its vmalloc arena at four gigabytes
    and its linear map at six. Both are empty on this machine and both are
    well under the 512GB a three-level page table can address, which matters
    because arch/lkl uses three levels and a virtual address that does not fit
    is one Linux indexes its own tables with out of range.

    nk reserves the same window in `paging::reserve_linux_window`, and the two
    have to agree.

    Matched by tokens rather than by exact text: the file separates a #define
    from its value with tabs, and a patch that has to reproduce them exactly
    is a patch that breaks on whitespace nobody can see.
    """
    _vmalloc_arena()
    _linear_map()


def define(path, values):
    """Set `#define NAME ...` to the value wanted, for each name given.

    Every run, and only rewriting when something differs. The tree is a docker
    volume that survives between builds, so a patch here is a migration rather
    than an edit: one that only knows how to apply itself can never be changed
    afterwards, and deleting the line that set an address does not put the old
    one back.

    Matched by tokens, because these headers separate a name from its value
    with tabs and a patch that has to reproduce invisible whitespace is one
    that breaks on whitespace nobody can see.
    """
    s = open(path).read()
    out, seen, changed = [], set(), False
    for line in s.split(chr(10)):
        word = line.split()
        if len(word) >= 3 and word[0] == '#define' and word[1] in values:
            seen.add(word[1])
            fixed = '#define ' + word[1] + ' ' + values[word[1]]
            changed = changed or fixed != line
            out.append(fixed)
        else:
            out.append(line)
    missing = set(values) - seen
    if missing:
        sys.exit('patch-lkl: ' + path + ' has no ' + ', '.join(sorted(missing)))
    if changed:
        open(path, 'w').write(chr(10).join(out))
    return changed


def _vmalloc_arena():
    """Move everything Linux places by address into the window nk reserved.

    The definitions that matter are the MMU ones. arch/lkl's pgtable.h has a
    VMALLOC_START of its own, and it is in the `#ifndef CONFIG_MMU` half --
    patching that one changes nothing and looks like it worked. With MMU the
    values come from pgtable-mmu-3level.h, where VMALLOC_START is
    `memory_end + VMALLOC_OFFSET`: directly on top of Linux's memory, which on
    nk is inside the identity map of RAM.

    STACK_TOP is the same problem from the other side -- it is
    CONFIG_LKL_MEMORY_START minus a little, so it lands just *below* the linear
    map, also in nk's RAM.
    """
    changed = define('arch/lkl/include/asm/pgtable-mmu-3level.h', {
        'VMALLOC_START': VMALLOC_BASE,
        'VMALLOC_END': VMALLOC_TOP,
    })
    changed |= define('arch/lkl/include/asm/processor.h', {
        'STACK_TOP': STACK_TOP,
        'STACK_TOP_MAX': STACK_TOP,
    })
    if changed:
        say("Linux's vmalloc arena and stack moved into nk's window")


def _linear_map():
    """Set arch/lkl's memory-layout defaults to the ones nk agrees with.

    Both are hex symbols with no prompt, so Kconfig takes their value from the
    `default` line and ignores anything .config says -- silently, which looks
    exactly like an assignment that worked.

    Written every run rather than once, because the tree is a docker volume
    that survives between builds: a patch here is a migration, not an edit, and
    one that only knows how to apply itself cannot be changed later. Deleting
    the line that set an address does not put the old one back, and the symptom
    of that is a device programmed with an address that used to be right.

    LKL_MEMORY_START is the linear map, and it is a *physical* address because
    LKL's __pa() is the identity -- a physical address is the virtual address
    it was mapped at. On a host that is a Unix process nothing notices, since
    nothing does real DMA. nk hands Linux real hardware, so this has to be
    memory that is really there, and nk reserves exactly this range.
    """
    path = 'arch/lkl/Kconfig'
    s = open(path).read()
    want = {'LKL_MEMORY_START': MEMORY_START, 'LKL_TASK_UNMAPPED_BASE': TASK_BASE}
    out, seen, changed, symbol = [], 0, False, None
    for line in s.split(chr(10)):
        word = line.split()
        if len(word) == 2 and word[0] == 'config':
            symbol = word[1]
        if len(word) == 2 and word[0] == 'default' and symbol in want:
            seen += 1
            fixed = line.split('default')[0] + 'default ' + want[symbol]
            changed = changed or fixed != line
            out.append(fixed)
        else:
            out.append(line)
    if seen != len(want):
        sys.exit('patch-lkl: expected ' + str(len(want)) + ' defaults in ' + path)
    if changed:
        open(path, 'w').write(chr(10).join(out))
        say("Linux's memory layout set to nk's")


def console_driver(shim):
    """A console Linux owns.

    nk answered writes to descriptors 1 and 2 itself, which works for a program
    that prints and is exactly wrong for a shell: `ls > file` is a dup2 of a
    file onto descriptor 1, and nk would have gone on writing to the UART.
    """
    dst = 'arch/lkl/drivers/nk-console.c'
    shutil.copy(f'{shim}/lkl/nk-console.c', dst)
    mk = 'arch/lkl/drivers/Makefile'
    if 'nk-console' not in open(mk).read():
        open(mk, 'a').write('obj-y += nk-console.o\n')
        say('arch/lkl builds nk-console.c')


def thread_files():
    """Let a thread share its creator's descriptor table.

    `new_host_task()` clones every host task from host0, never from the
    caller, so nk has to `unshare(CLONE_FILES)` to give a *process* a table of
    its own. A thread needs the opposite and cannot get it the same way:
    CLONE_FILES is part of what `pthread_create` asks for, and by the time nk
    knows whose thread this is, the task already exists -- there is no clone
    left to pass a flag to.

    Two failures come out of a thread holding a private copy, and they look
    nothing like each other. A descriptor opened after the thread started is
    invisible to it (EBADF). And a close in one thread is not a close: the
    creator's duplicate keeps the file open, so a socket a worker finished
    with never sends FIN and the peer waits for an EOF that cannot arrive --
    which is a hung HTTP response, and is what stopped a ThreadingHTTPServer
    on nk while the identical single-threaded sequence completed.

    So: adopt the table rather than copy it. One syscall, taking the pid to
    share with, called by the new thread on its own way up.
    """
    if 'sys_share_files' in open('arch/lkl/kernel/syscalls.c').read():
        return
    edit('arch/lkl/include/uapi/asm/unistd.h',
         '#define __NR_new_thread_group_leader\t(__NR_arch_specific_syscall + 1)',
         '#define __NR_new_thread_group_leader\t(__NR_arch_specific_syscall + 1)\n'
         '#define __NR_share_files\t\t(__NR_arch_specific_syscall + 2)',
         'share_files syscall number')
    edit('arch/lkl/include/asm/unistd.h',
         '__SYSCALL(__NR_new_thread_group_leader, sys_new_thread_group_leader)',
         '__SYSCALL(__NR_new_thread_group_leader, sys_new_thread_group_leader)\n'
         '__SYSCALL(__NR_share_files, sys_share_files)',
         'share_files in the syscall table')
    edit('arch/lkl/kernel/syscalls.c',
         'static asmlinkage long sys_new_thread_group_leader(void);',
         'static asmlinkage long sys_new_thread_group_leader(void);\n\n'
         'static asmlinkage long sys_share_files(long pid);',
         'share_files declared')
    edit('arch/lkl/kernel/syscalls.c',
         '#include <linux/task_work.h>',
         '#include <linux/task_work.h>\n#include <linux/fdtable.h>\n'
         '#include <linux/sched/task.h>',
         'share_files includes')
    open('arch/lkl/kernel/syscalls.c', 'a').write(SHARE_FILES_C)
    say('arch/lkl can share a descriptor table between threads')


SHARE_FILES_C = chr(10) + chr(10) + """
/*
 * nk: adopt another task's descriptor table.
 *
 * The thread this runs in was cloned from host0 and then given a table of
 * its own; what it needs is the one its creator is using. There is no clone
 * left to pass CLONE_FILES to, so take a reference to theirs and drop ours.
 *
 * Refcount first, swap second, release last: the old table must not be freed
 * while anything still points at it, and taking the reference before the swap
 * means a creator that exits between the two cannot take the table with it.
 */
SYSCALL_DEFINE1(share_files, long, pid)
{
	struct task_struct *t;
	struct files_struct *theirs, *mine;

	rcu_read_lock();
	t = find_task_by_pid_ns((pid_t)pid, &init_pid_ns);
	if (t)
		get_task_struct(t);
	rcu_read_unlock();
	if (!t)
		return -ESRCH;

	task_lock(t);
	theirs = t->files;
	if (theirs)
		atomic_inc(&theirs->count);
	task_unlock(t);
	put_task_struct(t);

	if (!theirs)
		return -ESRCH;
	if (theirs == current->files) {
		/* Already sharing: give back the reference just taken. */
		put_files_struct(theirs);
		return 0;
	}

	task_lock(current);
	mine = current->files;
	current->files = theirs;
	task_unlock(current);

	if (mine)
		put_files_struct(mine);
	return 0;
}
"""


if __name__ == '__main__':
    shim = sys.argv[1] if len(sys.argv) > 1 else '/shim'
    arm64_open_flags()
    linux_address_space()
    host_user_access()
    host0_signals()
    thread_files()
    console_driver(shim)
