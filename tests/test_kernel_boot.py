"""Boot nk on QEMU and check what it says.

The only test a kernel can have before it has an allocator: run the real
thing on the real emulator and read the serial console. Skipped rather than
failed where cargo or qemu is absent, so this does not break `python3 -m
unittest discover` on a machine that only works on the desktop.
"""

import re
import shutil
import struct
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUN = ROOT / 'scripts/run-kernel.sh'
BIN = ROOT / 'kernel/target/aarch64-unknown-none-softfloat/release/nk.bin'

# rustup from Homebrew is keg-only, so cargo is not on a default PATH; the
# same two directories run-kernel.sh adds.
CARGO = shutil.which('cargo') or shutil.which('cargo', path='/opt/homebrew/opt/rustup/bin')
HAVE = bool(CARGO) and bool(shutil.which('qemu-system-aarch64'))


def boot(*args, timeout=60, watchdog=12):
    out = subprocess.run(
        ['bash', str(RUN), '--timeout', str(watchdog), *args],
        capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL,
    )
    return out.stdout + out.stderr


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

    def test_lkl_user_pointer_boundary(self):
        # This used to assert `syscall 56 is not implemented`, which was the
        # old contract: pointer-bearing calls were refused outright. openat
        # has a descriptor now, so the assertion had become a claim that the
        # feature was absent. What still holds -- and what the boundary is
        # actually for -- is that a *kernel* address is refused whoever asks.
        self.assertIn('refused a user pointer into kernel memory (EFAULT)', self.out)
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

    def test_an_undescribed_syscall_is_refused_and_named(self):
        # 40 is mount: four pointers, no descriptor. Forwarding it
        # unmarshalled would hand Linux user addresses it cannot safely hold.
        # Naming the number is how the list of what to describe next gets
        # written by a real binary rather than guessed at.
        self.assertIn('syscall 40 has no descriptor yet', self.out)

    def test_nested_pointers_are_walked_not_passed(self):
        # readv fills two disjoint user buffers from one flat kernel buffer,
        # and the fixture checks the distribution itself: the first iovec has
        # length 1 and must receive only the 0x7f, the second must receive
        # "ELF". Getting that wrong exits 99 rather than printing this, and
        # writev carries it back out as three gathered pieces.
        self.assertIn('and again by readv, gathered back out with writev: ELF', self.out)

    def test_nothing_faulted(self):
        self.assertNotIn('!!EXC', self.out)
        self.assertNotIn('!! kernel panic', self.out)
        self.assertIn('nk: done.', self.out)


NET_LIB = ROOT / 'kernel/ldk/build/virtio-net/libnklinux.a'


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
@unittest.skipUnless(NET_LIB.exists(), 'virtio-net port not built')
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


if __name__ == '__main__':
    unittest.main()
