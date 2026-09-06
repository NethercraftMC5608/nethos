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


def boot(*args, timeout=60):
    out = subprocess.run(
        ['bash', str(RUN), '--timeout', '10', *args],
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


if __name__ == '__main__':
    unittest.main()
