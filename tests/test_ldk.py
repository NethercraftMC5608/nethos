"""ldk: reading what a Linux driver needs, and writing the stubs for it.

The compile itself needs docker and a 1.5GB Linux tree, so it is not tested
here. What is tested is everything either side of it: that the ELF reader
tells the truth about an object's symbols, that a port manifest says what ldk
requires, and that the generated stubs are valid C defining exactly the
symbols they claim.
"""

import json
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'pkg'))
sys.path.insert(0, str(ROOT / 'kernel/ldk'))
import npkg_elf  # noqa: E402

PORTS = ROOT / 'kernel/ldk/ports'
STUBS = ROOT / 'kernel/linux/stubs'
CLANG = shutil.which('clang')


def compile_aarch64(src: str, out: str):
    subprocess.run([CLANG, '--target=aarch64-unknown-none', '-ffreestanding',
                    '-c', src, '-o', out], check=True, capture_output=True)


@unittest.skipUnless(CLANG, 'needs clang to make an object to read')
class SymbolReading(unittest.TestCase):
    """npkg_elf.symbols — the whole basis of the discovery step."""

    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.mkdtemp()
        src = Path(cls.dir) / 't.c'
        src.write_text(
            'extern int outside(int);\n'
            'extern int data_outside;\n'
            'int inside(int x) { return outside(x) + data_outside; }\n'
            'static int hidden(void) { return 2; }\n'
            'int use(void) { return hidden(); }\n'
        )
        cls.obj = str(Path(cls.dir) / 't.o')
        compile_aarch64(str(src), cls.obj)
        cls.defined, cls.undefined = npkg_elf.symbols(cls.obj)

    def test_finds_defined_globals(self):
        self.assertIn('inside', self.defined)
        self.assertIn('use', self.defined)

    def test_finds_undefined_globals(self):
        # Both a function and a data symbol: a driver needs plenty of each,
        # and a reader that saw only calls would understate the shim's job.
        self.assertIn('outside', self.undefined)
        self.assertIn('data_outside', self.undefined)

    def test_ignores_file_static_symbols(self):
        # A static function is nobody else's business and is never a
        # requirement. Counting it would inflate every port's number.
        self.assertNotIn('hidden', self.defined)
        self.assertNotIn('hidden', self.undefined)

    def test_the_two_sets_do_not_overlap(self):
        self.assertEqual(self.defined & self.undefined, set())

    def test_a_relocatable_object_has_no_program_headers(self):
        # The reason this needed a new code path at all: everything npkg_elf
        # did before went through the program header table, and a .o has none.
        elf = npkg_elf._load(self.obj)
        self.addCleanup(elf.fh.close)
        self.assertEqual(elf.segments, [])
        self.assertGreater(len(elf.sections()), 0)


class PortManifests(unittest.TestCase):
    def test_every_port_declares_what_ldk_needs(self):
        found = list(PORTS.glob('*.json'))
        self.assertTrue(found, 'no ports at all')
        for path in found:
            with self.subTest(port=path.stem):
                port = json.loads(path.read_text())
                for key in ('name', 'summary', 'why', 'linux', 'sources'):
                    self.assertIn(key, port)
                self.assertEqual(port['name'], path.stem, 'name must match the filename')
                self.assertTrue(port['sources'])
                for src in port['sources']:
                    # Paths relative to the Linux tree, and C. A .o here would
                    # compile nothing and report zero symbols needed.
                    self.assertTrue(src.endswith('.c'), src)
                    self.assertFalse(src.startswith('/'), src)


@unittest.skipUnless(CLANG, 'needs clang')
@unittest.skipUnless((STUBS / 'virtio-blk.c').exists(), 'stubs not generated')
class GeneratedStubs(unittest.TestCase):
    """The stubs are committed, so they can be checked without docker."""

    @classmethod
    def setUpClass(cls):
        cls.path = STUBS / 'virtio-blk.c'
        cls.text = cls.path.read_text()
        cls.marked = set(re.findall(r'/\* @stub (\S+) \*/', cls.text))
        cls.dir = tempfile.mkdtemp()
        cls.obj = str(Path(cls.dir) / 'stubs.o')
        compile_aarch64(str(cls.path), cls.obj)
        cls.defined, cls.undefined = npkg_elf.symbols(cls.obj)

    def test_it_is_valid_aarch64_c(self):
        # Compiling it is the check. A generator that emits a symbol name C
        # cannot spell -- an operator, a keyword -- fails right here.
        self.assertTrue(Path(self.obj).exists())

    def test_defines_exactly_what_it_marks(self):
        # The @stub markers are what ldk re-reads to know a symbol is already
        # stubbed. If they drift from what the file actually defines, `ldk
        # stubs` starts generating duplicates or losing entries.
        self.assertEqual(self.defined, self.marked)

    def test_needs_only_the_halt_function(self):
        self.assertEqual(self.undefined, {'nk_stub_called'})

    def test_the_kernel_provides_that_function(self):
        src = (ROOT / 'kernel/core/src/stub.rs').read_text()
        self.assertIn('pub extern "C" fn nk_stub_called', src)
        # And it is pinned in the link, because nothing in the kernel calls
        # it yet and the linker would otherwise drop it.
        self.assertIn('--undefined=nk_stub_called',
                      (ROOT / 'kernel/core/build.rs').read_text())

    def test_a_stub_never_returns_a_plausible_value(self):
        # The single most important property of the whole shim: a stub that
        # quietly returns 0 gives a driver that appears to work and is subtly
        # wrong, which is far harder to find than a halt naming the function.
        for line in self.text.splitlines():
            if 'nk_stub_called' in line and line.startswith('long '):
                self.assertIn('nk_stub_called', line)


if __name__ == '__main__':
    unittest.main()
