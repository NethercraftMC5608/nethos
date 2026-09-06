"""Exercise the kernel's ELF validator directly, without QEMU or LKL."""
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RUSTC = shutil.which('rustc') or shutil.which('rustc', path='/opt/homebrew/opt/rustup/bin')


@unittest.skipUnless(RUSTC, 'needs rustc')
class KernelElf(unittest.TestCase):
    def test_loader_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'elf-tests'
            subprocess.run([RUSTC, '--edition=2021', '--test',
                            str(ROOT / 'kernel/core/src/elf.rs'), '-o', str(binary)],
                           check=True, capture_output=True, text=True)
            result = subprocess.run([str(binary)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
