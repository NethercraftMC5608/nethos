"""Boot nk on QEMU and check what it says.

The only test a kernel can have before it has an allocator: run the real
thing on the real emulator and read the serial console. Skipped rather than
failed where cargo or qemu is absent, so this does not break `python3 -m
unittest discover` on a machine that only works on the desktop.
"""

import re
import shutil
import struct
import os
import signal
import subprocess
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / 'scripts/run-kernel.sh'
BIN = ROOT / 'kernel/target/aarch64-unknown-none-softfloat/release/nk.bin'

# rustup from Homebrew is keg-only, so cargo is not on a default PATH; the
# same two directories run-kernel.sh adds.
CARGO = shutil.which('cargo') or shutil.which('cargo', path='/opt/homebrew/opt/rustup/bin')
HAVE = bool(CARGO) and bool(shutil.which('qemu-system-aarch64'))


# What nk prints on its last line, and what it prints instead when it dies.
# A run is over at either: waiting past them buys nothing and costs the whole
# watchdog, which is most of what this suite used to spend its time doing.
DONE = ('nk: done.', '!! kernel panic')


def boot(*args, timeout=60, watchdog=12, keys=None):
    """Boot nk and return everything it said.

    Reads the serial console as it arrives and stops at nk's own end marker
    rather than waiting for QEMU to exit. nk asks the firmware to switch the
    machine off when it finishes and QEMU under HVF does not oblige, so
    without this every class pays its full watchdog -- twelve seconds for a
    run that takes one, ninety for a run that takes fifteen.
    """
    # --no-build when the runner has already built every variant. Cargo takes
    # a lock on the package cache that is global, not per target directory, so
    # a dozen classes starting at once queue on it -- and a class whose
    # watchdog is twelve seconds can spend all twelve waiting for a build it
    # did not ask for.
    prebuilt = ['--no-build'] if os.environ.get('NK_TESTS_PREBUILT') else []
    # Its own process group. QEMU is a grandchild -- run-kernel.sh backgrounds
    # it and waits -- so terminating the shell leaves QEMU holding the pipe,
    # and anything that reads to end-of-file then waits for QEMU's watchdog,
    # which is exactly the wait this function exists to avoid.
    proc = subprocess.Popen(
        ['bash', str(RUN), '--timeout', str(watchdog), *prebuilt, *args],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, start_new_session=True,
        stdin=subprocess.PIPE if keys is not None else subprocess.DEVNULL,
    )
    if keys is not None:
        # Written and closed before reading a line: the whole point is that it
        # arrives before anything has opened the console, which is what a pipe
        # does and what nk has to cope with.
        proc.stdin.write(keys)
        proc.stdin.close()
    lines = []
    deadline = time.monotonic() + timeout
    try:
        # readline, not `for line in proc.stdout`. Iterating a file object
        # uses a read-ahead buffer, so on a pipe it hands back nothing until
        # several kilobytes have arrived -- which for a kernel whose whole
        # output is a few kilobytes means nothing until QEMU exits, which is
        # the thing this loop exists to avoid waiting for.
        for line in iter(proc.stdout.readline, ''):
            lines.append(line)
            if any(marker in line for marker in DONE):
                break
            if time.monotonic() > deadline:
                break
    finally:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
        # Whatever is still in the pipe, without waiting for end-of-file: the
        # group has been told to go, and a reader that insists on EOF is back
        # to waiting for the slowest thing in it.
        try:
            proc.stdout.close()
        except OSError:
            pass
        proc.wait(timeout=10)
    return ''.join(lines)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class KernelBoot(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.out = boot()

    def test_reaches_rust_and_names_itself(self):
        # The banner is printed from rust_main, so seeing it at all means
        # boot.s got through the image header, the EL check, the stack, the
        # BSS and the vector table.
        self.assertIn('NETHOS kernel (nk)', self.out)
        # Stage-independent on purpose: this asserted "Stage 0 reached" and
        # broke the moment Stage 1 changed the wording, which is a test
        # failing for a reason that is not a bug.
        self.assertRegex(self.out, r'Stage \d')

    def test_runs_at_el1(self):
        # boot.s drops from EL2 if firmware left it there. Everything from
        # Stage 1 on -- VBAR_EL1, TTBR, the generic timer -- assumes EL1, and
        # arriving at the wrong exception level is silent until one of them
        # traps.
        self.assertIn('running at EL1', self.out)

    def test_is_handed_a_device_tree(self):
        # The whole of what nk is told about the machine. A zero here means
        # QEMU took the ELF path instead of the arm64 Linux one, which is
        # exactly the bug the image header in boot.s exists to fix.
        m = re.search(r'device tree at (0x[0-9a-f]+)', self.out)
        self.assertIsNotNone(m, f'no device tree line in:\n{self.out}')
        self.assertNotEqual(int(m.group(1), 16), 0, 'no DTB passed -- image header wrong?')

    def test_never_faults_on_the_way(self):
        self.assertNotIn('!! exception', self.out)
        self.assertNotIn('!! kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Stage1(unittest.TestCase):
    """Memory, interrupts and the scheduler, checked from the serial console."""

    @classmethod
    def setUpClass(cls):
        cls.out = boot()

    def test_mmu_is_actually_on(self):
        # Read back from SCTLR_EL1, not inferred from the kernel still
        # running: under an identity map a kernel that failed to enable the
        # MMU behaves identically until something needs a cache.
        m = re.search(r'sctlr_el1 (0x[0-9a-f]+)\s+M=(\d) C=(\d) I=(\d)', self.out)
        self.assertIsNotNone(m, f'no sctlr line in:\n{self.out}')
        self.assertEqual((m.group(2), m.group(3), m.group(4)), ('1', '1', '1'))

    def test_reserves_the_kernel_and_the_device_tree(self):
        # Not a round number: whatever is left after the image and the DTB are
        # held back. A kernel that handed out its own pages would report the
        # full count here and fail later, somewhere else entirely.
        m = re.search(r'frames: \d+ of (\d+) pages', self.out)
        self.assertIsNotNone(m)
        total = int(m.group(1))
        self.assertLess(total, 512 * 1024 // 4, 'nothing was reserved')
        self.assertGreater(total, 500 * 1024 // 4, 'far too much was reserved')

    def test_heap_passes_its_own_checks(self):
        self.assertIn('heap ok', self.out, 'the boot-time heap self-test did not pass')

    def test_gic_and_timer_come_up(self):
        self.assertIn('gic:    v3 up', self.out)
        self.assertRegex(self.out, r'timer:  100 Hz on PPI 27')

    def test_it_powers_off_rather_than_being_killed(self):
        self.assertIn('nk: done.', self.out)

    def test_two_threads_alternate_under_preemption(self):
        # The whole point of Stage 1. Neither worker yields: each spins until
        # the tick count moves, so every switch between them is involuntary.
        order = re.findall(r'\] (ping|pong) #(\d+)', self.out)
        self.assertGreaterEqual(len(order), 16, f'threads did not run:\n{self.out}')
        # Strictly alternating, and each counter strictly increasing -- a lost
        # callee-saved register or a switch onto the wrong stack shows up here
        # as a counter that repeats or jumps.
        for i, (name, n) in enumerate(order[:16]):
            self.assertEqual(name, 'ping' if i % 2 == 0 else 'pong', 'threads did not alternate')
            self.assertEqual(int(n), i // 2 + 1, 'a thread lost its local state across a switch')

    def test_both_threads_finish(self):
        self.assertIn('ping done', self.out)
        self.assertIn('pong done', self.out)

    def test_slices_are_shared_out(self):
        # Round robin over three runnable tasks. Nothing should be starved,
        # and nothing should be getting all of it.
        slices = [int(n) for n in re.findall(r'(\d+) slices', self.out)]
        self.assertGreaterEqual(len(slices), 3)
        self.assertGreater(min(slices), 0, 'a task was starved')
        self.assertLess(max(slices) - min(slices), 5, f'slices badly skewed: {slices}')

    def test_no_faults_or_spurious_interrupts(self):
        self.assertNotIn('!! exception', self.out)
        self.assertNotIn('!! kernel panic', self.out)
        self.assertNotIn('!! unexpected interrupt', self.out)


@unittest.skipUnless(BIN.exists(), 'kernel not built')
class ImageHeader(unittest.TestCase):
    """The 64 bytes that decide whether a bootloader will talk to us."""

    @classmethod
    def setUpClass(cls):
        b = BIN.read_bytes()[:64]
        (cls.code0, _, cls.text_offset, cls.image_size,
         cls.flags, _, _, _, cls.magic, _) = struct.unpack('<IIQQQQQQII', b)

    def test_magic(self):
        self.assertEqual(self.magic, 0x644d5241, 'not "ARM\\x64"')

    def test_load_address_matches_the_linker_script(self):
        # linker.ld links at 0x40080000 and nk is not position-independent, so
        # this offset above the start of virt's RAM has to be exactly 512KB.
        self.assertEqual(self.text_offset, 0x80000)

    def test_fixed_placement(self):
        # Bit 3 set would tell the bootloader it may load us anywhere, which
        # for a non-PIE kernel means anywhere wrong.
        self.assertEqual(self.flags & (1 << 3), 0)

    def test_first_instruction_branches_past_the_header(self):
        self.assertEqual(self.code0 >> 26, 0b000101, 'code0 is not an unconditional b')
        self.assertGreaterEqual((self.code0 & 0x03ffffff) * 4, 64)

    def test_image_size_covers_bss_and_the_boot_stack(self):
        # It declares the memory the bootloader must not put anything in, so
        # it is larger than the file: BSS and the 64KB boot stack occupy no
        # bytes on disk and every byte of them in RAM.
        self.assertGreater(self.image_size, len(BIN.read_bytes()))


PORT_LIB = ROOT / 'kernel/ldk/build/virtio-blk/libnklinux.a'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(PORT_LIB.exists(), 'virtio-blk port not built (cd kernel/ldk && ldk shim virtio-blk)')
class Stage3(unittest.TestCase):
    """An unmodified Linux driver, reading a real disk."""

    DISK = '/tmp/nk-test.img'
    MARKER = b'NETHOS nk: read by an unmodified Linux virtio_blk driver.\n'

    @classmethod
    def setUpClass(cls):
        # 4MiB, with a readable marker in sector 0. Written here rather than
        # committed: it is four megabytes of mostly zeroes and trivially
        # reproducible.
        img = bytearray(4 * 1024 * 1024)
        img[:len(cls.MARKER)] = cls.MARKER
        with open(cls.DISK, 'wb') as fh:
            fh.write(img)
        cls.out = boot('--port', 'virtio-blk', '--disk', cls.DISK, timeout=120)

    def test_the_drivers_initcalls_run(self):
        # module_init on a built-in driver is an entry in a .initcallN.init
        # section, gathered in level order by linker.ld. Zero here means the
        # sections were dropped and no driver ever registered.
        m = re.search(r'linux:\s+(\d+) initcalls ran', self.out)
        self.assertIsNotNone(m, f'no initcall line:\n{self.out}')
        self.assertGreaterEqual(int(m.group(1)), 3)

    def test_printk_formats_correctly(self):
        # %u worked and %x did not for a while, because hex_asc_upper -- a
        # lookup table -- had been stubbed as a function. Every driver
        # message was unreadable at exactly the point they were the only
        # diagnostic available.
        self.assertIn('printk check: u=42 d=-7 x=0xabcd s=ok', self.out)

    def test_the_driver_probes_the_device(self):
        # Printed by virtio_blk itself, not by nk: proof the unmodified
        # driver negotiated features and read the device's config space.
        self.assertRegex(self.out, r'\[linux\] virtio\d+: \[vd\w+\] \d+ 512-byte logical blocks')

    def test_capacity_matches_the_real_disk(self):
        m = re.search(r'virtio-blk is up -- (\d+) sectors', self.out)
        self.assertIsNotNone(m, f'no block device appeared:\n{self.out}')
        self.assertEqual(int(m.group(1)), 4 * 1024 * 1024 // 512)

    def test_it_powers_the_machine_off_cleanly(self):
        # A kernel killed by the watchdog and one that finished look identical
        # from outside -- both end with a signal after N seconds -- and that
        # ambiguity cost real time on one silent hang.
        self.assertIn('nk: done.', self.out)
        self.assertNotIn('terminating on signal', self.out)

    def test_it_reads_the_actual_bytes_off_the_disk(self):
        # The whole of Stage 3. Everything between the request and this data
        # is the real driver: the virtio header, the descriptor chain, the
        # notify register, its own interrupt handler, the used ring.
        self.assertIn('NETHOS nk: read by an unmodified', self.out)
        # And the hexdump agrees with the text, so the buffer really holds it
        # rather than the marker being echoed from somewhere else.
        self.assertIn('4e 45 54 48 4f 53', self.out)

    def test_the_buffer_was_actually_written(self):
        # It is prefilled with 0xAA. A read that never reaches the buffer
        # leaves it that way -- which is what happened while sg_phys was
        # wrong, and reported success the whole time.
        self.assertNotIn('aa aa aa aa aa aa aa aa', self.out)

    def test_no_stub_was_reached(self):
        self.assertNotIn('unimplemented Linux API', self.out)
        self.assertNotIn('!! exception', self.out)
        self.assertNotIn('!! kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class UserSpace(unittest.TestCase):
    """A program at EL0, in its own address space, making Linux syscalls.

    Runs in the build with no Linux port linked, which is where the demo is.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot()

    def test_a_process_runs_where_real_binaries_are_linked(self):
        # 0x400000 is where aarch64 links a non-PIE executable. Getting here
        # took two things: mapping only the 34MB of devices the machine
        # actually has instead of the whole first gigabyte, and marking user
        # pages non-global with a per-process ASID -- without which the
        # kernel's own constant walking of the low half left TLB entries that
        # poisoned the process's translations, but only under HVF.
        self.assertRegex(self.out, r'user:\s+\d+ bytes of program at 0x400000')

    def test_each_address_space_has_its_own_asid(self):
        # TTBR0[63:48] is the ASID, so a non-zero value here means the
        # hardware can tell this address space from the kernel's rather than
        # relying on a full TLB flush at every switch.
        m = re.search(r'ttbr0 (0x[0-9a-f]+)', self.out)
        self.assertIsNotNone(m)
        self.assertNotEqual(int(m.group(1), 16) >> 48, 0, 'ASID is still zero')

    def test_a_process_gets_its_own_address_space(self):
        m = re.search(r'user:\s+(\d+) bytes of program at (0x[0-9a-f]+).*ttbr0 (0x[0-9a-f]+)',
                      self.out)
        self.assertIsNotNone(m, f'no user process:\n{self.out}')
        # Its own translation table, not the kernel's.
        self.assertNotEqual(int(m.group(3), 16), 0)

    def test_the_program_runs_at_el0_and_write_reaches_the_console(self):
        # Printed by the kernel on behalf of the process, through syscall 64
        # with a pointer the kernel had to translate itself.
        self.assertIn('hello from EL0 -- this is user space, on nk.', self.out)

    def test_the_kernel_refuses_a_user_pointer_into_kernel_memory(self):
        # The whole point of two privilege levels. The process asks the kernel
        # to write out eight bytes of the kernel image; user_to_phys
        # translates with EL0's permissions, the translation fails, and write
        # returns -EFAULT. A kernel that printed its own memory to whoever
        # asked would say nothing here.
        self.assertIn('refused a user pointer into kernel memory (EFAULT)', self.out)

    def test_an_unimplemented_syscall_names_itself(self):
        # Without Linux linked in, nk's own table has two entries. The program
        # asks for getpid first; the answer is -ENOSYS, and the number is
        # logged -- which is how the list of what to implement next gets
        # written by a real binary rather than guessed at.
        self.assertIn('syscall 172 is not implemented', self.out)
        self.assertIn('the process exited with status -38', self.out)

    def test_exit_ends_the_process_cleanly(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('!!EXC', self.out)
        self.assertIn('nk: done.', self.out)


LKL_LIB = ROOT / 'kernel/ldk/build/lkl/libnklkl.a'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(LKL_LIB.exists(), 'Linux not built (cd kernel/ldk && ldk lkl)')
class LinuxOnNk(unittest.TestCase):
    """The whole Linux kernel, booting on nk, answering system calls."""

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--lkl', timeout=300, watchdog=60)

    def test_linux_boots(self):
        self.assertRegex(self.out, r'Linux version 6\.\d+')

    def test_it_gets_its_memory_from_nk(self):
        # Through the host's page_alloc, which is nk's frame allocator. The
        # range printed is inside nk's RAM.
        self.assertRegex(self.out, r'Memory: \d+K/\d+K available')

    def test_the_console_is_nks(self):
        # Every line above and below came through lkl_host_ops.print, which
        # is nk's UART.
        self.assertIn('printk: legacy console [lkl_console0] enabled', self.out)

    def test_the_subsystems_that_matter_come_up(self):
        # These are the ones a desktop needs and nk was never going to write.
        self.assertIn('NET: Registered PF_INET protocol family', self.out)
        self.assertIn('io scheduler mq-deadline registered', self.out)

    def test_it_reaches_init(self):
        # start_kernel completed. There is no filesystem yet, so there is no
        # /init to run -- but Linux got as far as looking for one.
        self.assertIn('Run /init as init process', self.out)

    def test_system_calls_are_answered_by_linux(self):
        # pid 1, because Linux's init task is what called it.
        self.assertRegex(self.out, r'getpid\(\)\s+-> 1')
        self.assertRegex(self.out, r'getuid\(\)\s+-> 0')

    def test_each_el0_process_has_its_own_linux_task(self):
        tasks = re.findall(r'process: nk (\d+) Linux pid (\d+) tid (\d+)', self.out)
        self.assertEqual(len(tasks), 2, self.out)
        self.assertNotEqual(tasks[0][0], tasks[1][0])
        self.assertNotEqual(tasks[0][1], tasks[1][1])
        for _, pid, tid in tasks:
            self.assertGreater(int(pid), 1)
            self.assertEqual(pid, tid)
            self.assertIn(f'the process exited with status {pid}', self.out)

    def test_private_linux_state_and_task_cleanup(self):
        self.assertIn('distinct PIDs, private files/fs, both Linux tasks reaped', self.out)
        self.assertIn('parent: Linux init survived both exits', self.out)
        self.assertIn('nk page tables, pages and task stacks reclaimed', self.out)

    def test_user_address_spaces_survive_preemption(self):
        self.assertRegex(self.out, r'EL0 IRQs [1-9]\d* and [1-9]\d*, private stacks survived')
        self.assertNotIn('the process exited with status 99', self.out)

    def test_elf_is_loaded_through_linux_vfs(self):
        self.assertEqual(self.out.count('rootfs: /nk-init read back through Linux VFS'), 2)
        self.assertEqual(self.out.count('ELF: 1 PT_LOAD segment(s)'), 2)

    def test_a_kernel_pointer_is_refused_by_linux(self):
        # The fixture writes to descriptor 1 from an address inside the kernel
        # image and requires EFAULT. It used to be nk that refused it, by
        # looking at the descriptor number; the process has a real console
        # now, so the write is Linux's and the refusal comes back out of
        # useraccess.rs. The fixture exits 99 if it does not.
        self.assertNotIn('the process exited with status 99', self.out)
        self.assertIn('hello from EL0 -- this is user space, on nk.', self.out)

    def test_a_process_reads_a_file_through_linuxs_vfs(self):
        # The marshalling layer, end to end. Every pointer in this -- the
        # path handed to openat, the buffer handed to read -- is a user
        # address that nk copied across rather than giving to Linux, because
        # Linux blocks inside syscalls and TTBR0 changes underneath it.
        #
        # "ELF" is the first three printable bytes of /nk-init, which is the
        # process's own executable, read back out of Linux's rootfs.
        self.assertIn('opened and read from EL0 through Linux, begins: ELF', self.out)

    def test_linux_refuses_a_kernel_pointer_from_el0(self):
        # openat's path is read by Linux itself now, through nk's translation,
        # so this goes all the way down Linux's own strncpy_from_user and back
        # out through useraccess.rs. The fixture asks it to open the kernel
        # image and exits 99 if the answer is anything but EFAULT.
        self.assertNotIn('the process exited with status 99', self.out)

    def test_nested_pointers_are_walked_not_passed(self):
        # readv fills two disjoint user buffers from one flat kernel buffer,
        # and the fixture checks the distribution itself: the first iovec has
        # length 1 and must receive only the 0x7f, the second must receive
        # "ELF". Getting that wrong exits 99 rather than printing this, and
        # writev carries it back out as three gathered pieces.
        self.assertIn('and again by readv, gathered back out with writev: ELF', self.out)

    def test_the_stack_is_the_shape_a_libc_expects(self):
        # The fixture walks it itself -- argc, a NUL-terminated argv, past
        # envp's terminator into the auxiliary vector, and checks AT_PAGESZ
        # and that AT_RANDOM's sixteen bytes are not all zero. Getting any of
        # that wrong exits 99 instead of printing this.
        self.assertIn('zeroed registers, argc, argv, envp and a seeded auxv', self.out)

    def test_a_process_has_a_heap_and_can_map_memory(self):
        # brk and mmap are nk's own: LKL is one flat region with no user half,
        # so forwarding them would move Linux's break and return an address
        # this process cannot reach. The fixture stores to both and reads back.
        self.assertIn('brk grew and holds a value, mmap gave a zeroed page', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('!!EXC', self.out)
        self.assertNotIn('!! kernel panic', self.out)
        self.assertIn('nk: done.', self.out)


NET_LIB = ROOT / 'kernel/ldk/build/virtio-net/libnklinux.a'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(NET_LIB.exists(), 'virtio-net port not built')
def build_c(name, out):
    """Compile kernel/init/<name>.c in ldk's container to kernel/ldk/build/<out>.

    `out` must be unique to the caller. The classes run in parallel, and two
    of them compiling the same source to the same path means one reads a file
    the other is still writing -- which presents as a class failing every
    assertion about a program that ran perfectly well on its own.
    """
    (ROOT / 'kernel/ldk/build').mkdir(parents=True, exist_ok=True)
    try:
        build = subprocess.run(
            ['docker', 'run', '--rm', '-v', f'{ROOT}:/w', '-w', '/w', 'nethos-ldk',
             'gcc', '-static', '-O2', '-o', f'kernel/ldk/build/{out}',
             f'kernel/init/{name}.c'],
            capture_output=True, text=True, timeout=300)
    except (FileNotFoundError, subprocess.TimeoutExpired) as e:
        raise unittest.SkipTest(f'no ldk container: {e}')
    if build.returncode != 0:
        raise unittest.SkipTest(f'no aarch64 toolchain: {build.stderr.strip()[:200]}')
    return ROOT / 'kernel/ldk/build' / out


def busybox():
    """Debian's busybox-static, arm64, fetched once and shared.

    Three classes want it and they run in parallel, so it is fetched into a
    file named for the process doing the fetching and moved into place -- a
    rename is atomic, and a half-written 2MB binary is not something the
    reader can detect.
    """
    out = ROOT / 'kernel/ldk/build/busybox'
    if out.exists():
        return out
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(f'.{os.getpid()}')
    try:
        bb = subprocess.run(
            ['docker', 'run', '--rm', '-v', f'{ROOT}/kernel/ldk/build:/out',
             'nethos-ldk', 'sh', '-c',
             'apt-get update -qq >/dev/null 2>&1;'
             ' apt-get install -y -qq busybox-static >/dev/null 2>&1;'
             f' cp /bin/busybox /out/{tmp.name}'],
            capture_output=True, text=True, timeout=600)
    except (FileNotFoundError, subprocess.TimeoutExpired) as e:
        raise unittest.SkipTest(f'no ldk container: {e}')
    if bb.returncode != 0 or not tmp.exists():
        raise unittest.SkipTest(f'no busybox: {bb.stderr.strip()[:200]}')
    tmp.replace(out)
    return out


def make_cpio(root, name):
    """Pack a directory as a newc cpio archive and return its path."""
    archive = ROOT / 'kernel/ldk/build' / name
    names = '\n'.join(sorted(str(p.relative_to(root)) for p in root.rglob('*'))) + '\n'
    with open(archive, 'wb') as out:
        cpio = subprocess.run(['cpio', '-o', '-H', 'newc'], cwd=root,
                              input=names.encode(), stdout=out,
                              stderr=subprocess.PIPE)
    if cpio.returncode != 0:
        raise unittest.SkipTest(f'cpio failed: {cpio.stderr.decode()[:200]}')
    return archive


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Interactive(unittest.TestCase):
    """A shell that reads what somebody types.

    The input is written and the pipe closed before nk has finished booting,
    which is the case that matters: a flip buffer with no tty behind it takes
    every byte and delivers none, so anything typed before init opens the
    console has to wait in the host's ring rather than be handed over early.
    """

    SCRIPT = ('#!/bin/busybox sh\n'
              'echo "nk shell ready"\n'
              'while read -r line; do\n'
              '  echo "you typed: $line"\n'
              '  [ "$line" = quit ] && break\n'
              'done\n'
              'echo goodbye\n')

    @classmethod
    def setUpClass(cls):
        bb = busybox()
        root = ROOT / 'kernel/ldk/build/interactive-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'bin').mkdir(parents=True)
        (root / 'dev').mkdir()
        shutil.copy(bb, root / 'bin/busybox')
        init = root / 'nk-init'
        init.write_text(cls.SCRIPT)
        init.chmod(0o755)
        cls.out = boot('--lkl', '--initrd', str(make_cpio(root, 'interactive.cpio')),
                       keys='hello there\nquit\n', timeout=240, watchdog=120)

    def test_the_shell_starts(self):
        self.assertIn('nk shell ready', self.out)

    def test_it_reads_what_was_typed(self):
        self.assertIn('you typed: hello there', self.out)
        self.assertIn('you typed: quit', self.out)

    def test_it_acts_on_it(self):
        # The loop breaks on "quit", so this is the shell having read the
        # second line and compared it, not just echoed it.
        self.assertIn('goodbye', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Shebang(unittest.TestCase):
    """An init that is a shell script, which is what an init usually is.

    A script is not a thing a loader can enter: the file names the program
    that can read it. Linux resolves that in binfmt_script and nk does the
    same, including the part that matters most here -- argv[0] is discarded
    and the script's own path becomes the interpreter's first argument. That
    is why `#!/bin/busybox sh` works where a plain copy of busybox at
    /nk-init exits 127: busybox picks its applet from basename(argv[0]).
    """

    SCRIPT = ('#!/bin/busybox sh\n'
              'echo "hello from a shell script, on nk"\n'
              'echo "argv0 is $0"\n'
              'echo written > /tmp/from-script\n'
              'busybox cat /tmp/from-script\n')

    @classmethod
    def setUpClass(cls):
        bb = busybox()
        root = ROOT / 'kernel/ldk/build/shebang-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'bin').mkdir(parents=True)
        (root / 'dev').mkdir()
        (root / 'tmp').mkdir()
        shutil.copy(bb, root / 'bin/busybox')
        init = root / 'nk-init'
        init.write_text(cls.SCRIPT)
        init.chmod(0o755)
        cls.out = boot('--lkl', '--initrd', str(make_cpio(root, 'shebang.cpio')),
                       timeout=240, watchdog=120)

    def test_the_script_runs(self):
        self.assertIn('hello from a shell script, on nk', self.out)

    def test_the_scripts_path_becomes_argv0(self):
        # binfmt_script discards the caller's argv[0] and puts the script
        # there instead, which is how a script knows its own name.
        self.assertIn('argv0 is /nk-init', self.out)

    def test_the_script_redirects_and_reads_back(self):
        self.assertIn('written', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class PersistentDisk(unittest.TestCase):
    """A real filesystem on real storage, surviving a reboot.

    Everything nk has written until now lived in a memory-backed rootfs and
    went when the machine did. This is ext4 on a virtio-blk disk: nk finds the
    device in its own device tree and hands it to Linux, Linux's own ext4
    mounts it, and a program at EL0 reads a file that was put there by
    mke2fs on the host and appends one of its own.

    The test boots twice on the same image, which is the only way to tell
    persistence from a filesystem that merely worked.
    """

    IMAGE = ROOT / 'kernel/ldk/build/root.img'

    @classmethod
    def setUpClass(cls):
        bb = ROOT / 'kernel/ldk/build/busybox'
        if not bb.exists():
            raise unittest.SkipTest('busybox not built; run the Busybox class first')

        # The image, built by mke2fs -d, which populates a filesystem from a
        # directory without needing to mount anything or be root.
        src = ROOT / 'kernel/ldk/build/root-src'
        shutil.rmtree(src, ignore_errors=True)
        (src / 'etc').mkdir(parents=True)
        (src / 'etc/greeting').write_text('this file lives on a real disk\n')
        cls.IMAGE.unlink(missing_ok=True)
        try:
            mk = subprocess.run(
                ['docker', 'run', '--rm', '-v', f'{ROOT}:/w', '-w', '/w', 'nethos-ldk',
                 'sh', '-c',
                 'command -v mke2fs >/dev/null ||'
                 ' { apt-get update -qq >/dev/null 2>&1;'
                 '   apt-get install -y -qq e2fsprogs >/dev/null 2>&1; };'
                 ' mke2fs -q -t ext4 -d kernel/ldk/build/root-src -F'
                 ' kernel/ldk/build/root.img 64M'],
                capture_output=True, text=True, timeout=600)
        except (FileNotFoundError, subprocess.TimeoutExpired) as e:
            raise unittest.SkipTest(f'no ldk container: {e}')
        if mk.returncode != 0:
            raise unittest.SkipTest(f'mke2fs failed: {mk.stderr.strip()[:200]}')

        root = ROOT / 'kernel/ldk/build/disk-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'bin').mkdir(parents=True)
        (root / 'dev').mkdir()
        (root / 'mnt').mkdir()
        shutil.copy(bb, root / 'bin/busybox')
        init = root / 'nk-init'
        init.write_text(
            '#!/bin/busybox sh\n'
            'busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null\n'
            'busybox mount -t ext4 /dev/vda /mnt || {\n'
            '  echo "root: mount failed"; exit 1; }\n'
            'echo "root: ext4 mounted from /dev/vda"\n'
            'busybox cat /mnt/etc/greeting\n'
            'echo "root: boot log so far:"\n'
            'busybox cat /mnt/etc/boots 2>/dev/null || echo "   (none: first boot)"\n'
            'echo "a boot happened" >> /mnt/etc/boots\n'
            'busybox sync\n'
            'busybox umount /mnt\n'
            'echo "root: done"\n')
        init.chmod(0o755)
        cpio = make_cpio(root, 'disk.cpio')

        cls.first = boot('--lkl', '--disk', str(cls.IMAGE), '--initrd', str(cpio),
                         timeout=240, watchdog=120)
        cls.second = boot('--lkl', '--disk', str(cls.IMAGE), '--initrd', str(cpio),
                          timeout=240, watchdog=120)

    def test_ext4_mounts_from_the_disk(self):
        self.assertIn('root: ext4 mounted from /dev/vda', self.first)

    def test_it_reads_a_file_put_there_by_the_host(self):
        self.assertIn('this file lives on a real disk', self.first)

    def test_the_first_boot_finds_no_log(self):
        self.assertIn('(none: first boot)', self.first)

    def test_the_second_boot_reads_what_the_first_wrote(self):
        # The whole point: this line is on the disk because a program running
        # on nk put it there, on a previous boot of the machine.
        self.assertIn('a boot happened', self.second)
        self.assertNotIn('(none: first boot)', self.second)

    def test_only_one_init_runs(self):
        # A kernel starts one init. nk runs a second process only for its own
        # fixture, where the point is to check that two of them are
        # independent; a supplied init running twice raced itself for the
        # disk.
        self.assertEqual(self.first.count('root: ext4 mounted from /dev/vda'), 1)
        self.assertNotIn('root: mount failed', self.first)

    def test_nothing_faulted(self):
        for out in (self.first, self.second):
            self.assertNotIn('fault in user space', out)
            self.assertNotIn('kernel panic', out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Gpu(unittest.TestCase):
    """A GPU, driven by Linux, on nk.

    virtio-gpu is a virtio-mmio device like the disk: nk finds it in its own
    device tree, hands it to Linux with virtio_mmio_device_add, and routes its
    interrupt. What comes back is DRM -- card0, a render node, a connector
    with modes, and an fbdev a program can write pixels to.

    Nothing is scanned out yet: no mode has been set on the CRTC, so QEMU
    reports "Display output is not active". That is a KMS modeset away and is
    the next piece of work; everything below it is here.
    """

    @classmethod
    def setUpClass(cls):
        bb = ROOT / 'kernel/ldk/build/busybox'
        if not bb.exists():
            raise unittest.SkipTest('busybox not built; run the Busybox class first')
        root = ROOT / 'kernel/ldk/build/gpu-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'bin').mkdir(parents=True)
        (root / 'dev').mkdir()
        shutil.copy(bb, root / 'bin/busybox')
        shutil.copy(build_c('fbtest', 'fbtest'), root / 'bin/fbtest')
        init = root / 'nk-init'
        init.write_text('#!/bin/busybox sh\n'
                        'busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null\n'
                        'busybox ls /dev/dri\n'
                        '/bin/fbtest\n')
        init.chmod(0o755)
        cls.out = boot('--lkl', '--gpu', '--initrd', str(make_cpio(root, 'gpu.cpio')),
                       timeout=240, watchdog=120)

    def test_nk_hands_the_gpu_to_linux(self):
        self.assertRegex(self.out, r'virtio: device 16 at 0x[0-9a-f]+ -> Linux')

    def test_the_driver_initialises(self):
        self.assertIn('Initialized virtio_gpu', self.out)

    def test_the_drm_nodes_exist(self):
        # renderD128 is the one Mesa opens.
        self.assertIn('card0', self.out)
        self.assertIn('renderD128', self.out)

    def test_a_program_can_draw_on_the_framebuffer(self):
        # Through write(), not mmap: a DRM dumb buffer has to be mapped and a
        # shared file-backed mapping is not something nk can do yet.
        self.assertRegex(self.out, r'fb: \d+x\d+ at 32 bpp')
        self.assertRegex(self.out, r'fb: drew \d+ lines')

    def test_the_display_is_turned_on(self):
        # Writing pixels fills a shadow buffer; nothing is scanned out until a
        # mode is set on the CRTC. FBIOPUT_VSCREENINFO is how fbdev asks for
        # that -- the DRM helper turns it into a modeset -- and without it
        # QEMU reports "Display output is not active" over a black screen.
        self.assertIn('fb: mode set, display should be active', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Busybox(unittest.TestCase):
    """A program nobody wrote for nk, doing something real.

    busybox is the useful test precisely because it is indifferent to us: a
    widely used, statically linked Linux binary that will make whatever calls
    it needs and report an errno when one is missing. `ls -l /` reaches the
    filesystem, the directory reader, stat, and the terminal ioctls, and
    prints something a person can check.
    """

    @classmethod
    def setUpClass(cls):
        busybox()
        root = ROOT / 'kernel/ldk/build/busybox-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'bin').mkdir(parents=True)
        # /dev for nk to put the console node in, /tmp for the shell to
        # redirect into. An empty directory has no other way into a cpio
        # archive than being there when it is built.
        (root / 'dev').mkdir()
        (root / 'tmp').mkdir()
        shutil.copy(build_c('busybox-init', 'bbinit'), root / 'nk-init')
        shutil.copy(ROOT / 'kernel/ldk/build/busybox', root / 'bin/busybox')
        cls.out = boot('--lkl', '--initrd', str(make_cpio(root, 'busybox.cpio')),
                       timeout=240, watchdog=120)

    def test_execve_starts_it(self):
        self.assertIn('execve: replaced this process', self.out)

    def test_the_shell_redirects_into_a_file(self):
        # `echo redirected > /tmp/out` is a dup2 of a file onto descriptor 1.
        # While nk answered descriptor 1 by its number the file stayed empty
        # and the word went to the UART; the only way to tell the difference
        # is to read the file back, which is what `busybox cat` here does.
        self.assertIn('redirected', self.out)

    def test_it_forks_children_that_inherit_the_console(self):
        # sh runs each command in a child. They print, so they have the
        # console; a child with an empty descriptor table would not.
        self.assertRegex(self.out, r'fork: child \d+ inherited [3-9]\d* descriptors')

    def test_it_lists_the_filesystem(self):
        # Its own output, in its own format, from Linux's rootfs.
        self.assertRegex(self.out, r'drwxr-xr-x +\d+ +0 +0 .* bin')
        self.assertRegex(self.out, r'-rwxr-xr-x +\d+ +0 +0 +\d+ .* nk-init')

    def test_it_exits_successfully(self):
        self.assertIn('the process exited with status 0', self.out)

    def test_no_syscall_was_refused(self):
        # With Linux doing its own user access there is no table to be missing
        # an entry from, so a real program should not meet a wall at all.
        self.assertNotIn('no descriptor', self.out)
        self.assertNotIn('is not implemented', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Fork(unittest.TestCase):
    """A process with a child, and a parent that reaps it.

    fork is nk's because the address space is: the child is a copy of the
    parent at the instruction it forked on, entered by restoring the parent's
    exception frame with x0 set to zero rather than by jumping to an entry
    point. wait4 is nk's because the exit status is -- Linux's own task is
    torn down with do_exit(0) underneath.
    """

    @classmethod
    def setUpClass(cls):
        root = ROOT / 'kernel/ldk/build/fork-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'etc').mkdir(parents=True)
        shutil.copy(build_c('fork', 'nk-fork'), root / 'nk-init')
        (root / 'etc/nk-greeting').write_text('inherited\n')
        cls.out = boot('--lkl', '--initrd', str(make_cpio(root, 'fork.cpio')),
                       timeout=180, watchdog=90)

    def test_the_child_sees_zero_and_the_parent_a_pid(self):
        self.assertIn('child: fork() returned 0', self.out)
        self.assertIn('parent: fork() returned a pid', self.out)

    def test_they_do_not_share_memory(self):
        # The same variable, written on both sides after the fork. A shared
        # page would make one of these read the other's value.
        self.assertIn('child: fork() returned 0, shared is 20', self.out)
        self.assertIn('parent: fork() returned a pid, shared is 10', self.out)

    def test_the_child_inherits_its_parents_descriptors(self):
        # The parent opens a file before forking and the child reads through
        # the same descriptor number. LKL clones every task from its own init,
        # never from the caller, so this is pidfd_open plus pidfd_getfd rather
        # than anything the task model gives us.
        self.assertRegex(self.out, r'fork: child \d+ inherited \d+ descriptors')
        self.assertIn('child: read "inherite" through a descriptor its parent'
                      ' opened', self.out)

    def test_the_parent_reaps_the_child_and_reads_its_status(self):
        self.assertIn('parent: reaped its child, which exited 9', self.out)

    def test_waiting_with_no_children_says_so(self):
        self.assertIn('parent: wait4 with no children returned ECHILD', self.out)

    def test_the_parent_exits_with_its_own_status(self):
        self.assertIn('the process exited with status 7', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Execve(unittest.TestCase):
    """One program replacing itself with another, in the same process.

    The hard part is the order. argv and envp live in the address space being
    replaced, so they have to be copied out before the replacement is built;
    and the kernel is mapped through the same tables as the process, so the
    old address space cannot be freed until TTBR0 points at the new one.
    """

    @classmethod
    def setUpClass(cls):
        parent = build_c('exec-parent', 'exec-parent')
        child = build_c('exec-child', 'exec-child')
        root = ROOT / 'kernel/ldk/build/exec-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'bin').mkdir(parents=True)
        shutil.copy(parent, root / 'nk-init')
        shutil.copy(child, root / 'bin/second')
        cls.out = boot('--lkl', '--initrd', str(make_cpio(root, 'exec.cpio')),
                       timeout=180, watchdog=90)

    def test_a_failed_execve_leaves_the_caller_intact(self):
        # execve promises this, and it is why nk validates the image and
        # builds the new address space before tearing down the old one.
        self.assertIn('survived a failed execve, errno was ENOENT', self.out)

    def test_the_program_is_replaced(self):
        self.assertIn('execve: replaced this process', self.out)

    def test_the_new_program_gets_its_arguments(self):
        # These pointers were the *parent's* memory, in an address space that
        # no longer exists by the time the child reads them.
        self.assertIn('child: argc 3, argv[0]=/bin/second, argv[1]=and,'
                      ' argv[2]=its arguments', self.out)

    def test_the_new_program_gets_its_environment(self):
        self.assertIn('child: getenv(NK) is the environment survived too', self.out)

    def test_it_exits_with_the_new_programs_status(self):
        self.assertIn('the process exited with status 9', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class Initrd(unittest.TestCase):
    """A userland that is not part of the kernel image.

    The boot loader leaves a cpio archive in RAM and names the range in
    /chosen; nk reserves those pages, unpacks the archive into Linux's rootfs,
    and runs what it finds. That is what makes a program on nk something other
    than a fixture compiled into nk.bin.
    """

    @classmethod
    def setUpClass(cls):
        root = ROOT / 'kernel/ldk/build/initrd-root'
        shutil.rmtree(root, ignore_errors=True)
        (root / 'etc').mkdir(parents=True)
        shutil.copy(build_c('hello', 'hello-for-initrd'), root / 'nk-init')
        (root / 'etc/nk-greeting').write_text(
            'a userland that is not part of the kernel image\n')
        cls.out = boot('--lkl', '--initrd', str(make_cpio(root, 'initrd.cpio')),
                       timeout=180, watchdog=90)

    def test_the_archive_is_found_and_unpacked(self):
        self.assertRegex(self.out, r'initrd: \d+ entries unpacked into Linux')

    def test_the_program_comes_from_the_archive_not_the_kernel_image(self):
        self.assertIn('/nk-init came from the initrd', self.out)
        self.assertNotIn('read back through Linux VFS', self.out)

    def test_it_runs(self):
        self.assertIn('hello from a real compiled binary, on nk', self.out)

    def test_the_archives_other_files_landed_too(self):
        # Opened with fopen, from a directory the unpacker had to create.
        self.assertIn('/etc/nk-greeting says: a userland that is not part of'
                      ' the kernel image', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class RealBinary(unittest.TestCase):
    """A program compiled by a real toolchain, unmodified, at EL0.

    Everything else in this file runs a fixture written for nk. This one runs
    what `gcc -static` produces from ordinary C against ordinary glibc -- a
    compiler that has never heard of nk, targeting Linux's ABI, which is the
    only reason any of the rest of this project matters.

    Skipped when the ldk container is not available, because building it needs
    an aarch64 toolchain and this suite has to keep passing without docker.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--lkl', '--init', str(build_c('hello', 'nk-hello')),
                       timeout=180, watchdog=90)

    def test_it_runs_and_prints(self):
        self.assertIn('hello from a real compiled binary, on nk', self.out)

    def test_printf_and_malloc_work(self):
        # printf with arguments drags in malloc, stdio buffering and an fstat
        # on the console; the string came out of a heap allocation.
        self.assertIn('argv[0] is /nk-init, argc is 1, and the heap works too', self.out)

    def test_it_returns_from_main_through_glibcs_exit(self):
        # Returning from main runs glibc's atexit handlers -- including the
        # one it takes from x0 at process entry, which is why every register
        # has to arrive zero. See docs/KERNEL.md.
        self.assertIn('and its destructor ran on the way out', self.out)

    def test_it_exits_with_its_own_status(self):
        self.assertIn('the process exited with status 7', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('fault in user space', self.out)
        self.assertNotIn('kernel panic', self.out)


class Stage4(unittest.TestCase):
    """An unmodified Linux driver, sending and receiving a real packet.

    Under TCG, not HVF: virtio-net makes an MMIO access QEMU's HVF backend
    refuses to decode. Emulation is perhaps twenty times slower, hence the
    much longer watchdog.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--port', 'virtio-net', '--net', '--tcg',
                       timeout=300, watchdog=90)

    def test_the_vmemmap_is_mapped(self):
        # Without it, anything that dereferences a struct page faults on an
        # address nothing ever mapped -- which is where Stage 4 stopped.
        self.assertRegex(self.out, r'vmemmap: \d+ MiB of struct page at 0x[0-9a-f]+')

    def test_the_driver_reads_its_mac_off_the_device(self):
        # QEMU's default, so this is a real read of the device's config
        # space rather than anything nk invented.
        self.assertIn('virtio-net is up -- 52:54:00:12:34:56', self.out)

    def test_it_gets_an_arp_reply(self):
        # Transmit and receive both, through the real driver: the virtio
        # header, the descriptor chain, the notify register, its own
        # interrupt handler, NAPI, and the used ring.
        self.assertIn('10.0.2.2 is at 52:55:0a:00:02:02', self.out)

    def test_the_frame_really_is_an_arp_reply(self):
        # Checked from the bytes, not from nk's own summary of them:
        # destination is our MAC, ethertype 0806, opcode 0002.
        self.assertIn('52 54 00 12 34 56 52 55 0a 00 02 02 08 06 00 01', self.out)
        self.assertIn('08 00 06 04 00 02', self.out)

    def test_nothing_faulted_and_no_stub_was_reached(self):
        self.assertNotIn('unimplemented Linux API', self.out)
        self.assertNotIn('!!EXC', self.out)
        self.assertNotIn('!! kernel panic', self.out)
        self.assertIn('nk: done.', self.out)


ARCHIVE = ROOT / 'kernel/ldk/build/drm.cpio'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(ARCHIVE.exists(), 'run scripts/build-drm-test.sh first')
class DrmDlopen(unittest.TestCase):
    """The mechanism Mesa loads by, at a size that fits in memory.

    Mesa is not one library: libEGL dlopens libEGL_mesa, which dlopens a
    _dri.so, which dlopens libgallium, which needs libLLVM. Every step is a
    runtime open, mmap, relocate and TLS allocation. libLLVM alone is 118MB
    and Linux's pool on nk is 64, so Mesa itself cannot be in a rootfs that
    lives in RAM -- but that is a memory problem, and it is only worth
    solving if the loading works at all.

    So this proves the loading with libdrm, which is 132KB: a library that was
    never on the link line, opened at runtime, and used to make a real DRM
    ioctl against the virtio_gpu nk gave Linux. What comes back is the
    driver's own name.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--lkl', '--gpu', '--initrd', str(ARCHIVE),
                       timeout=240, watchdog=120)

    def test_a_program_can_mount_devtmpfs(self):
        # Not decoration: without it /dev is empty and the GPU that
        # demonstrably exists has no node to open.
        self.assertIn('DEVTMPFS_OK', self.out)

    def test_dlopen_finds_and_maps_a_library(self):
        self.assertIn('DLOPEN_OK', self.out)

    def test_symbols_resolve_out_of_it(self):
        self.assertIn('DLSYM_OK', self.out)

    def test_the_library_talks_to_the_gpu(self):
        # DRM_IOCTL_VERSION, answered by the unmodified virtio_gpu driver.
        self.assertIn('DRM_DRIVER virtio_gpu', self.out)

    def test_it_ran_to_the_end(self):
        self.assertIn('DRMPROBE_OK', self.out)


MESA_IMG = ROOT / 'kernel/ldk/build/mesa.img'
MESA_CPIO = ROOT / 'kernel/ldk/build/mesa.cpio'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(MESA_IMG.exists() and MESA_CPIO.exists(),
                     'run scripts/build-mesa-disk.sh first')
class Mesa(unittest.TestCase):
    """Mesa on nk: a GL context, and something rendered in it.

    Debian's own Mesa, unmodified, with llvmpipe. Software rasterisation is
    not a consolation prize here -- it is the part of Mesa that leans hardest
    on everything nk gained last (threads, dlopen, private file mappings, TLS
    in dlopened libraries) and barely touches the GPU at all.

    It lives on an ext4 disk rather than in the initrd because libLLVM is
    118MB and Linux's pool on nk is 64. No switch_root was needed for that:
    nk's execve and dynamic loader both go through Linux's VFS, so a binary
    and its libraries on a mounted filesystem work as they are.

    Three things had to change in nk. A single mapping was capped at 64MB and
    libLLVM's text segment is 117MB. The user address space was 256MB with
    QEMU's devices still in the middle of it, leaving no contiguous run big
    enough. And file mappings were read a page at a time, which is thirty
    thousand round trips through ext4 and virtio-blk for that one segment.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--lkl', '--gpu', '--disk', str(MESA_IMG),
                       '--initrd', str(MESA_CPIO), timeout=600, watchdog=300)

    def test_the_disk_mounts(self):
        self.assertIn('mesa: disk mounted', self.out)

    def test_mesa_loads_and_initialises_egl(self):
        self.assertIn('EGL_INIT_OK', self.out)
        self.assertIn('EGL_VENDOR Mesa Project', self.out)

    def test_the_renderer_is_llvmpipe(self):
        # Which means libLLVM mapped: 117MB in one PT_LOAD, and the reason
        # for both the wider address space and the batched reads.
        self.assertIn('GL_RENDERER llvmpipe', self.out)

    def test_it_is_a_real_gles_context(self):
        self.assertRegex(self.out, r'GL_VERSION OpenGL ES 3\.\d')

    def test_it_rendered_and_the_pixels_came_back(self):
        # A context that was created and never drawn to proves the loader
        # worked, not the rasteriser. This is glReadPixels of a cleared
        # framebuffer, and the colour is one no uninitialised buffer would
        # plausibly hold.
        self.assertIn('GL_PIXEL 64 128 191 255', self.out)

    def test_it_ran_to_the_end(self):
        self.assertIn('MESA_OK', self.out)


NPKG_IMG = ROOT / 'kernel/ldk/build/npkg.img'
NPKG_CPIO = ROOT / 'kernel/ldk/build/npkg.cpio'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(NPKG_IMG.exists() and NPKG_CPIO.exists(),
                     'run scripts/build-npkg-disk.sh first')
class Npkg(unittest.TestCase):
    """NETHOS's package manager, on NETHOS's kernel.

    npkg is pure-stdlib Python, so this is really a CPython port -- and
    CPython is a harder test of a kernel than anything else nk runs. It
    dlopens forty extension modules, threads through concurrent.futures,
    maps its stdlib, and installs signal handlers on the way up. nk has no
    signals; Linux records the handler and nk never delivers one, which for
    a package manager means no Ctrl-C and nothing else.

    The repository is a local directory, so the install is entirely offline
    and still the real thing: resolve dependencies, verify sha256, unpack,
    record in the database. nk has no network, and this does not need one.

    The one thing that had to change was not in nk at all. Debian's python3
    is ET_EXEC rather than a PIE, so it must land at the 0x400000 baked into
    it -- and invoking the loader by hand, `ld.so /path/to/python3`, puts the
    loader there first and the mapping fails. Executed directly it works,
    because nk reads PT_INTERP and places the interpreter out of the way.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--lkl', '--disk', str(NPKG_IMG),
                       '--initrd', str(NPKG_CPIO), timeout=400, watchdog=200)

    def test_cpython_runs(self):
        self.assertRegex(self.out, r'PYTHON_OK 3\.\d+\.\d+')

    def test_npkg_reads_an_empty_database(self):
        self.assertIn('no packages installed', self.out)

    def test_it_resolves_a_dependency_and_orders_the_install(self):
        # nk-tools depends on nk-greeting and only nk-tools was asked for, so
        # a package manager that cannot solve prints one name here, not two.
        self.assertIn('Installing: nk-greeting-1.0, nk-tools-2.1', self.out)
        self.assertLess(self.out.index('installed nk-greeting'),
                        self.out.index('installed nk-tools'))

    def test_both_packages_are_installed(self):
        self.assertIn('installed nk-greeting-1.0-1 (1 files)', self.out)
        self.assertIn('installed nk-tools-2.1-1 (2 files)', self.out)

    def test_the_database_can_be_read_back(self):
        self.assertIn('nk-tools', self.out)
        self.assertIn('/usr/share/nk/notes/README', self.out)

    def test_it_knows_which_package_owns_a_path(self):
        self.assertIn('/usr/share/nk/greeting is owned by nk-greeting',
                      self.out)

    def test_the_installed_files_verify(self):
        self.assertIn('all packages verify', self.out)

    def test_the_unpacked_file_is_really_there(self):
        # The end of it: bytes that came out of a tarball npkg unpacked,
        # read back off the filesystem by something that is not npkg.
        self.assertIn('installed by npkg, running on nk', self.out)

    def test_it_ran_to_the_end(self):
        self.assertIn('npkg: exit 0', self.out)


SOAK_CPIO = ROOT / 'kernel/ldk/build/soak.cpio'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(SOAK_CPIO.exists(),
                     'run scripts/build-soak-test.sh first')
class SyscallSoak(unittest.TestCase):
    """The desktop's syscalls, before the desktop.

    socketpair and SCM_RIGHTS are the Wayland socket, epoll is the
    compositor's event loop, eventfd wakes its threads, memfd is wl_shm,
    poll is everything that waits the other way. All of them forward to
    Linux untouched -- nk owns none of these numbers -- except the memfd
    mmap, which is nk's sys_mmap and refuses MAP_SHARED. That refusal is
    the test working: the one marker that must fail until MAP_SHARED
    exists, proving the probe can see the gap it was built to find.

    arch/lkl's defconfig leaves CONFIG_UNIX off, so the first run of
    this failed at socketpair with EAFNOSUPPORT. The forwarding was
    fine; the family was simply not compiled in. kernel/lkl/nk.config
    now switches on UNIX, TMPFS and MEMFD_CREATE.
    """

    @classmethod
    def setUpClass(cls):
        cls.out = boot('--lkl', '--initrd', str(SOAK_CPIO),
                       timeout=120, watchdog=30)

    def test_socketpair_sends_and_receives(self):
        self.assertEqual(self.out.count('SOAK_SOCKETPAIR_OK'), 1, self.out)

    def test_scm_rights_passes_a_live_fd(self):
        self.assertEqual(self.out.count('SOAK_SCM_RIGHTS_OK'), 1, self.out)

    def test_epoll_wakes_on_a_pipe(self):
        self.assertEqual(self.out.count('SOAK_EPOLL_OK'), 1, self.out)

    def test_eventfd_counts(self):
        self.assertEqual(self.out.count('SOAK_EVENTFD_OK'), 1, self.out)

    def test_memfd_mmap_needs_shared(self):
        # MAP_SHARED is blocker #1. The probe reaches the mmap, nk
        # refuses it with EOPNOTSUPP, and the marker stays absent --
        # while everything around it passes. When MAP_SHARED lands,
        # this becomes an assertEqual on SOAK_MEMFD_OK like the rest.
        self.assertIn('mmap memfd: Operation not supported', self.out)
        self.assertNotIn('SOAK_MEMFD_OK', self.out)

    def test_poll_waits_on_a_pipe(self):
        # poll sits after memfd in soak.c, so it is not reached until
        # MAP_SHARED exists. Asserted absent for the same reason: the
        # run must stop exactly where the missing feature is.
        self.assertNotIn('SOAK_POLL_OK', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('!!EXC', self.out)
        self.assertNotIn('!! kernel panic', self.out)
        self.assertIn('nk: done.', self.out)


if __name__ == '__main__':
    unittest.main()
