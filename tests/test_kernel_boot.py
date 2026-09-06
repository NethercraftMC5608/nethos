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
        ['bash', str(RUN), '--timeout', '8', *args],
        capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL,
    )
    return out.stdout + out.stderr


@unittest.skipUnless(HAVE, 'needs cargo and qemu-system-aarch64')
class KernelBoot(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.out = boot()

    def test_reaches_rust_and_names_itself(self):
        self.assertIn('NETHOS kernel (nk)', self.out)
        self.assertIn('Stage 0 reached', self.out)

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
