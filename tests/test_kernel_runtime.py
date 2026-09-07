"""Debian dynamic-loader integration. Build with scripts/build-runtime-test.sh."""
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / 'kernel/ldk/build/runtime.cpio'


@unittest.skipUnless(ARCHIVE.exists(), 'run scripts/build-runtime-test.sh first')
class DynamicRuntime(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        result = subprocess.run(['bash', str(ROOT/'scripts/run-kernel.sh'), '--lkl',
                                 '--initrd', str(ARCHIVE), '--timeout', '20'],
                                capture_output=True, text=True, timeout=60)
        cls.output = result.stdout + result.stderr

    # Once each: nk starts one init, the way a kernel does. It ran two until
    # the second was recognised as belonging to nk's own fixture, where the
    # point is to check that two processes are independent.
    def test_debian_dynamic_libc(self):
        self.assertEqual(self.output.count('DYNAMIC_LIBC_OK'), 1, self.output)

    def test_private_file_mapping(self):
        self.assertEqual(self.output.count('PRIVATE_FILE_MMAP_OK'), 1, self.output)

    def test_fixed_mapping_and_protection(self):
        self.assertEqual(self.output.count('FIXED_MAPPING_OK'), 1, self.output)

    def test_nothing_faulted(self):
        self.assertNotIn('!!EXC', self.output)
        self.assertNotIn('!! kernel panic', self.output)
        self.assertIn('nk: done.', self.output)

    def test_pthread_tls_and_join(self):
        # A real pthread_create, a worker that changes shared state and its
        # own thread-local, a join, and a parent whose thread-local was left
        # alone. This was an expected failure until nk had CLONE_THREAD and a
        # futex of its own.
        self.assertNotIn('PTHREAD_CREATE_FAILED', self.output)
        self.assertEqual(self.output.count('PTHREAD_TLS_JOIN_OK'), 1, self.output)
