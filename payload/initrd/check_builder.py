"""Offline checks for the archive builder; run with python3 on any host."""
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def entries(path):
    data = path.read_bytes()
    result = {}
    offset = 0
    while True:
        assert data[offset:offset + 6] == b'070701'
        fields = [int(data[offset + 6 + i * 8:offset + 14 + i * 8], 16) for i in range(13)]
        start = offset + 110
        name = data[start:start + fields[11]]
        assert name[-1:] == b'\0'
        name = name[:-1].decode()
        start = (start + fields[11] + 3) & ~3
        body = data[start:start + fields[6]]
        assert len(body) == fields[6]
        offset = (start + fields[6] + 3) & ~3
        if name == 'TRAILER!!!':
            assert not any(data[offset:])
            return result
        assert name not in result
        result[name] = (fields, body)


class Builder(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='nk initrd ')
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.cache = self.directory / 'cached inputs'
        self.cache.mkdir()
        binary = bytearray(128)
        binary[:7] = b'\x7fELF\x02\x01\x01'
        struct.pack_into('<HHI', binary, 16, 2, 183, 1)
        struct.pack_into('<Q', binary, 32, 64)
        struct.pack_into('<HH', binary, 54, 56, 1)
        struct.pack_into('<I', binary, 64, 1)
        (self.cache / 'busybox').write_bytes(binary)
        (self.cache / 'applets').write_text('sh\ncat\nls\n[\n[[\nbusybox\n')
        (self.cache / 'version').write_text('test-version\n')
        self.output = self.directory / 'root fs.cpio'

    def build(self, *args):
        return subprocess.run(['bash', str(ROOT / 'scripts/build-initrd.sh'),
                               '--cache', str(self.cache), '--output', str(self.output), *args],
                              capture_output=True, text=True,
                              env=dict(os.environ, SOURCE_DATE_EPOCH='0'))

    def test_layout_modes_and_symlinks(self):
        run = self.build()
        self.assertEqual(run.returncode, 0, run.stderr)
        archive = entries(self.output)
        self.assertEqual(archive['tmp'][0][1], 0o41777)
        for name in ('bin', 'dev', 'etc', 'proc'):
            self.assertEqual(archive[name][0][1], 0o40755)
        self.assertNotIn('dev/console', archive)
        self.assertEqual(archive['etc/hostname'][1], b'nethos\n')
        for name in ('passwd', 'group', 'hostname'):
            self.assertEqual(archive['etc/' + name][0][1], 0o100644)
        for name in ('sh', 'cat', 'ls', '[', '[['):
            self.assertEqual(archive['bin/' + name][0][1], 0o120777)
            self.assertEqual(archive['bin/' + name][1], b'busybox')
        self.assertEqual(archive['bin/busybox'][0][1], 0o100755)
        self.assertEqual(archive['nk-init'][1], archive['bin/busybox'][1])
        self.assertEqual(archive['nk-init'][0][1], 0o100755)
        for fields, _ in archive.values():
            self.assertEqual(fields[2:4], [0, 0])
            self.assertEqual(fields[5], 0)

    def test_reproducible(self):
        self.assertEqual(self.build().returncode, 0)
        first = self.output.read_bytes()
        (self.cache / 'applets').write_text('[[\ncat\nsh\n[\nls\ncat\n')
        self.assertEqual(self.build().returncode, 0)
        self.assertEqual(self.output.read_bytes(), first)

    def test_copies(self):
        self.assertEqual(self.build('--copies').returncode, 0)
        archive = entries(self.output)
        self.assertEqual(archive['bin/cat'][0][1], 0o100755)
        self.assertEqual(archive['bin/cat'][1], archive['bin/busybox'][1])

    def test_wrong_architecture_preserves_output(self):
        self.output.write_bytes(b'previous archive')
        binary = bytearray((self.cache / 'busybox').read_bytes())
        struct.pack_into('<H', binary, 18, 62)
        (self.cache / 'busybox').write_bytes(binary)
        self.assertNotEqual(self.build().returncode, 0)
        self.assertEqual(self.output.read_bytes(), b'previous archive')

    def test_reject_dynamic_binary(self):
        binary = bytearray((self.cache / 'busybox').read_bytes())
        struct.pack_into('<I', binary, 64, 3)
        (self.cache / 'busybox').write_bytes(binary)
        self.assertNotEqual(self.build().returncode, 0)

    def test_reject_version_mismatch(self):
        self.assertNotEqual(self.build('--package-version', 'other-version').returncode, 0)
        self.assertFalse(self.output.exists())

    def test_reject_unsafe_applet(self):
        with (self.cache / 'applets').open('a') as f:
            f.write('../escape\n')
        self.assertNotEqual(self.build().returncode, 0)


if __name__ == '__main__':
    unittest.main()
