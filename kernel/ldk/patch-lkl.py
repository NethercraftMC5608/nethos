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


if __name__ == '__main__':
    shim = sys.argv[1] if len(sys.argv) > 1 else '/shim'
    arm64_open_flags()
    host_user_access()
    host0_signals()
    console_driver(shim)
