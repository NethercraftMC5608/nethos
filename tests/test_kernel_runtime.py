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

    def test_debian_dynamic_libc(self):
        self.assertEqual(self.output.count('DYNAMIC_LIBC_OK'), 2, self.output)

    def test_private_file_mapping(self):
        self.assertEqual(self.output.count('PRIVATE_FILE_MMAP_OK'), 2, self.output)

    def test_fixed_mapping_and_protection(self):
        self.assertEqual(self.output.count('FIXED_MAPPING_OK'), 2, self.output)

    def test_thread_attempt_does_not_crash_kernel(self):
        self.assertNotIn('!!EXC', self.output)
        self.assertNotIn('!! kernel panic', self.output)
        self.assertIn('nk: done.', self.output)
        self.assertTrue('PTHREAD_CREATE_FAILED: 38' in self.output or
                        'PTHREAD_TLS_JOIN_OK' in self.output, self.output)

    @unittest.expectedFailure
    def test_pthread_tls_and_join(self):
        # Real pthread_create currently reaches nk's explicit CLONE_THREAD
        # rejection. Keep the desired outcome visible, not a fake success.
        self.assertEqual(self.output.count('PTHREAD_TLS_JOIN_OK'), 2, self.output)
