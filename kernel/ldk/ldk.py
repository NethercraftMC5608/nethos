#!/usr/bin/env python3
"""
ldk — the Linux Driver Kit. What nk owes an unmodified Linux driver.

    ldk fetch                 a Linux tree, configured for arm64
    ldk build   <port>        compile the port's drivers, unmodified
    ldk syms    <port>        what they need that nk does not provide
    ldk stubs   <port>        write a stub for every one of those
    ldk shim    <port>        compile the shim and archive it with the drivers
    ldk report                coverage, across every port
    ldk ports                 what ports exist

There is no way to translate a Linux driver into another kernel's driver
model, and there cannot be a good one: Linux has no stable in-kernel API by
policy, so a driver is welded to the kernel's internals rather than written
against an interface. What does work -- Genode's dde_linux, LKL, rump kernels
-- is to keep the driver source byte-for-byte, compile it against Linux's own
headers, and implement underneath it only what the linker says is missing.

That is all this tool does, and the important consequence is:

    the shim is discovered, not designed.

You never sit down to implement "the Linux kernel API". You compile a driver,
read off a few hundred undefined symbols, stub every one of them to panic,
boot it, and implement only what it actually reaches. `ldk report` exists to
make that number visible, so the decision to attempt a new class of device is
made against a measurement rather than a feeling.

Two decisions worth knowing about:

**kbuild compiles the drivers, not us.** Reconstructing Linux's own include
paths and flags by hand is a large, silent source of wrongness -- a header
found in the wrong place gives a driver that compiles and behaves differently.
`make ARCH=arm64 drivers/virtio/virtio_mmio.o` uses exactly the flags Linux
would, and the question of whether we got them right does not arise.

**It runs in a container.** The Linux source tree cannot live on macOS: it
contains filenames differing only in case, and a case-insensitive volume
loses one of each pair on extraction. payload/bin/nethos-kernel already keeps
its tree in a docker volume for the same reason.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
KERNEL = HERE.parent
ROOT = KERNEL.parent
PORTS = HERE / "ports"
BUILD = HERE / "build"
STUBS = KERNEL / "linux" / "stubs"
EMUL = KERNEL / "linux" / "emul"

# pkg/ is the project's own ELF reader. Reused rather than reimplemented, and
# rather than shelling out to nm: the same file already answers this question
# for packages, and a .o is only an ELF with no program headers.
sys.path.insert(0, str(ROOT / "pkg"))
import npkg_elf  # noqa: E402

IMAGE = "nethos-ldk"
SRC_VOLUME = "nethos-ldk-src"
LKL_VOLUME = "nethos-lkl-src"
DEFAULT_VERSION = "7.2"

# Symbols the toolchain provides, not the kernel. Stubbing these would be
# actively wrong -- a generated __stack_chk_fail that returned would defeat
# the check it exists to make.
#
# Only genuinely toolchain-supplied things belong here. The string functions
# were in this list once, on the assumption that something would provide them;
# nothing did, and the link failed on strcmp and strcpy. They are implemented
# in emul/string.c now and counted like everything else, because a list of
# "somebody else's problem" that is wrong is worse than no list.
TOOLCHAIN = {
    # Rust's compiler_builtins supplies these for a bare-metal target.
    "memcpy", "memmove", "memset", "memcmp",
    # -fstack-protector-strong; emul/glue.c handles the failure path, and the
    # canary itself comes from SP_EL0 rather than a symbol -- see the
    # -mstack-protector-guard=sysreg flags kbuild passes on arm64.
    "__stack_chk_guard",
    "__stack_chk_fail",
    "__aeabi_unwind_cpp_pr0", "__aeabi_unwind_cpp_pr1",
}

# Everything nk itself exports to the shim is named nk_*, and the prefix is
# the boundary rather than a convention: a symbol with it is answered by the
# Rust side and must never be stubbed here. Without this rule the shim's own
# calls into nk get a generated stub each, and every one collides with the
# real thing at link time.
NK_PREFIX = "nk_"

# Provided by linker.ld, not by Linux and not by the shim. Stubbing these
# replaces the bounds of the initcall table with a function, so nk_linux_init
# walks from one piece of code to another and calls whatever it finds.
LINKER = {"__initcall_start", "__initcall_end", "__image_end", "__bss_start",
          "__bss_end", "__stack_top", "__vectors"}

# Linux source every port needs, whatever device it is for.
#
# Named once here rather than repeated in each manifest, because the list is a
# property of the shim -- printk needs vsprintf, vsprintf needs hexdump's
# tables -- and not of any particular driver. A port's own `sources` is then
# only the driver, which is what a reader wants it to be.
BASE_SOURCES = [
    "lib/vsprintf.c",     # %pS, %pOF and every other kernel-specific specifier
    "lib/hexdump.c",      # hex_asc_upper, which every %x in the log reads
    "lib/scatterlist.c",  # sg_init_table and friends, with the real edge cases
    "lib/ctype.c",        # _ctype, the table every isalpha() in the tree reads
    "lib/find_bit.c",     # _find_next_bit and friends, over cpumasks and more
]


def say(msg):
    print(f"\033[1;36m==>\033[0m {msg}", flush=True)


def die(msg):
    print(f"\033[1;31mERROR:\033[0m {msg}", file=sys.stderr)
    raise SystemExit(1)


def have_docker() -> bool:
    return shutil.which("docker") is not None


def docker(args, mounts=None, check=True, capture=False):
    if not have_docker():
        die("docker is missing. The Linux tree cannot live on macOS -- see the "
            "note at the top of ldk.py.  brew install colima docker && colima start")
    cmd = ["docker", "run", "--rm", "-v", f"{SRC_VOLUME}:/src"]
    for host, guest in (mounts or []):
        cmd += ["-v", f"{host}:{guest}"]
    cmd += [IMAGE, "sh", "-c", args]
    try:
        return subprocess.run(cmd, check=check, text=True, capture_output=capture)
    except subprocess.CalledProcessError:
        # The compiler has already said what is wrong, in its own words and
        # above this line. A Python traceback on top of it buries the errors
        # under the entire command line, which for a kbuild invocation is
        # about a hundred flags.
        die("the container step failed -- see the output above")


def load_port(name: str) -> dict:
    path = PORTS / f"{name}.json"
    if not path.exists():
        avail = ", ".join(sorted(p.stem for p in PORTS.glob("*.json")))
        die(f"no such port: {name}\n  have: {avail}")
    return json.loads(path.read_text())


# ---------------------------------------------------------------- fetch --

def cmd_fetch(args):
    """A Linux tree with an arm64 build directory prepared beside it.

    Its *own* tree, not the one payload/bin/nethos-kernel uses. That one
    carries a dirty in-tree x86 build, and an out-of-tree `O=` build refuses
    to start against an unclean source -- the fix for which is `make
    mrproper`, which would silently destroy another tool's working state.
    """
    version = args.version
    if not shutil.which("docker"):
        die("docker is missing (brew install colima docker && colima start)")

    say(f"Building the {IMAGE} image")
    subprocess.run(["docker", "build", "-q", "-t", IMAGE, str(HERE)],
                   check=True, stdout=subprocess.DEVNULL)

    subprocess.run(["docker", "volume", "create", SRC_VOLUME],
                   check=True, stdout=subprocess.DEVNULL)

    # The kernel tarball is large. If nethos-kernel has already fetched this
    # version into its own volume, take it from there rather than pulling
    # 150MB again -- and fall back to kernel.org when it has not.
    series = f"v{version.split('.')[0]}.x"
    script = f"""
set -e
if [ ! -d /src/linux-{version} ]; then
  if [ -f /shared/linux-{version}.tar.xz ]; then
    echo "using the tarball nethos-kernel already fetched"
    tar -C /src -xJf /shared/linux-{version}.tar.xz
  else
    echo "fetching linux {version} from kernel.org"
    curl -fL --retry 5 --retry-all-errors -o /src/linux-{version}.tar.xz \
      https://cdn.kernel.org/pub/linux/kernel/{series}/linux-{version}.tar.xz
    tar -C /src -xJf /src/linux-{version}.tar.xz
  fi
fi
cd /src/linux-{version}
if [ ! -f /src/build-arm64/include/generated/autoconf.h ]; then
  echo "configuring arm64"
  make O=/src/build-arm64 ARCH=arm64 defconfig >/dev/null
  echo "make prepare"
  make O=/src/build-arm64 ARCH=arm64 prepare -j$(nproc) >/dev/null
fi
echo "ready: $(ls /src/build-arm64/include/generated/autoconf.h)"
"""
    say(f"Preparing linux {version} for arm64 (a few minutes the first time)")
    cmd = ["docker", "run", "--rm",
           "-v", f"{SRC_VOLUME}:/src",
           "-v", "nethos-kernel-src:/shared:ro",
           IMAGE, "sh", "-c", script]
    subprocess.run(cmd, check=True)
    say("Tree ready.")


# ---------------------------------------------------------------- build --

def sources_of(port: dict) -> list[str]:
    """The port's own drivers, plus the Linux library every port needs."""
    out = list(port["sources"])
    for src in BASE_SOURCES:
        if src not in out:
            out.append(src)
    return out


def objects_of(port: dict) -> list[str]:
    return [s[:-2] + ".o" for s in sources_of(port)]


def cmd_build(args):
    port = load_port(args.port)
    version = port.get("linux", DEFAULT_VERSION)
    objs = objects_of(port)
    out = BUILD / port["name"]
    out.mkdir(parents=True, exist_ok=True)

    # The driver sources are not touched, not patched and not copied. That is
    # the entire premise: if a port ever needs to edit Linux source, the
    # approach has failed and the edit is hiding it.
    say(f"Compiling {len(objs)} objects, unmodified, with kbuild")
    script = f"""
set -e
cd /src/linux-{version}
for o in {' '.join(objs)}; do
  echo "  $o"
  make O=/src/build-arm64 ARCH=arm64 "$o" >/tmp/log 2>&1 || {{ tail -20 /tmp/log; exit 1; }}
  install -D "/src/build-arm64/$o" "/out/$(basename $o)"
done
"""
    docker(script, mounts=[(str(out), "/out")])
    say(f"Objects in {out.relative_to(ROOT)}")


# ----------------------------------------------------------------- syms --

def shim_symbols() -> set[str]:
    """What the shim already implements.

    Read from compiled objects when they exist -- the linker's own view, and
    the only one that cannot disagree with reality. Falls back to nothing,
    which is correct at the point the shim is still empty.
    """
    provided: set[str] = set()
    for obj in sorted((BUILD / "_shim").glob("*.o")):
        defined, _ = npkg_elf.symbols(str(obj))
        provided |= defined
    return provided


def stubbed_symbols(name: str) -> set[str]:
    """Symbols already carrying a generated stub.

    Parsed out of the generated file's own markers rather than by compiling
    it, so `ldk syms` works before anything has been built.
    """
    path = STUBS / f"{name}.c"
    if not path.exists():
        return set()
    out = set()
    for line in path.read_text().splitlines():
        if line.startswith("/* @stub "):
            out.add(line.split()[2])
    return out


def analyse(port: dict) -> dict:
    out = BUILD / port["name"]
    objs = sorted(out.glob("*.o"))
    if not objs:
        die(f"nothing built for {port['name']} -- run: ldk build {port['name']}")

    defined: set[str] = set()
    undefined: set[str] = set()
    for obj in objs:
        d, u = npkg_elf.symbols(str(obj))
        defined |= d
        undefined |= u

    # The shim's own undefined symbols count too, and missing this was a real
    # bug: implementing the platform bus introduced a call to
    # platform_get_resource, which is a genuine Linux function and not an
    # inline. Nothing generated a stub for it, because stubs were generated
    # from the drivers alone, and the link failed on a symbol no report
    # mentioned. Implementing part of Linux pulls in more of Linux, and the
    # accounting has to say so.
    shim_def: set[str] = set()
    shim_undef: set[str] = set()
    for obj in sorted((BUILD / "_shim").glob("*.o")):
        d, u = npkg_elf.symbols(str(obj))
        shim_def |= d
        shim_undef |= u

    # Which of them are *data*, not functions.
    #
    # An undefined ELF symbol carries no type, so the symbol table cannot say
    # whether virtio_check_mem_acc_cb is a function or a pointer to one. The
    # relocations can: a symbol reached by CALL26 is called, one with an
    # LDST_ABS_LO12 against it is read from, and one that is only ever read
    # from is data.
    #
    # This matters because getting it wrong is not a link error. Define a
    # function where a function pointer was wanted and the caller loads your
    # first eight bytes of machine code and jumps to them -- which is exactly
    # what happened, and the resulting fault address was instruction encoding
    # with nothing pointing back at the cause.
    called: set[str] = set()
    loaded: set[str] = set()
    for obj in objs + sorted((BUILD / "_shim").glob("*.o")):
        c, l = npkg_elf.references(str(obj))
        called |= c
        loaded |= l

    # A symbol one file in the port defines and another uses is internal to
    # the port and nobody's responsibility but its own.
    external = {s for s in (undefined | shim_undef) - defined - TOOLCHAIN - LINKER
                if not s.startswith(NK_PREFIX)}
    implemented = external & shim_def
    stubbed = (external - implemented) & stubbed_symbols(port["name"])
    missing = external - implemented - stubbed
    # Never called => treat as data.
    #
    # Stricter than "read from with an LDST relocation", and deliberately so:
    # a character lookup table is reached by adrp+add and then indexed with a
    # register, which is the same relocation pattern as taking a function's
    # address. hex_asc_upper slipped through the narrower rule and every %x
    # and negative %d in the kernel log printed rubbish.
    #
    # The wider rule is safe in both directions. A data stub is a pointer to a
    # panicking function: if the symbol really is a function pointer, calling
    # through it names the symbol; if it is plain data, the value is
    # meaningless but nothing jumps into it. A *function* stub for a data
    # symbol is the case that cannot be recovered from -- the caller loads
    # eight bytes of machine code and branches to them.
    data = external - called
    read_from = external & loaded
    return {
        "data": data,
        "read_from": read_from,
        "objects": [o.name for o in objs],
        "defined": defined,
        "external": external,
        "implemented": implemented,
        "stubbed": stubbed,
        "missing": missing,
        # What the shim asked for that the drivers never did.
        "shim_pulled": (shim_undef - undefined) - defined - TOOLCHAIN - shim_def,
    }


def cmd_syms(args):
    port = load_port(args.port)
    a = analyse(port)
    print()
    print(f"  {port['name']} — {port['summary']}")
    print(f"  {len(a['objects'])} objects, {len(a['defined'])} symbols defined")
    print()
    print(f"  needs {len(a['external'])} symbols from the kernel underneath it")
    if a["shim_pulled"]:
        print(f"    ({len(a['shim_pulled'])} of them asked for by the shim, not the drivers)")
    print(f"    implemented   {len(a['implemented'])}")
    print(f"    stubbed       {len(a['stubbed'])}")
    print(f"    not yet       {len(a['missing'])}")
    if a["data"]:
        print()
        print(f"  {len(a['data'])} are never called -- data, or a pointer to a function:")
        for sym in sorted(a["data"]):
            where = "implemented" if sym in a["implemented"] else "stubbed"
            read = ", read from" if sym in a["read_from"] else ""
            print(f"    {sym}  ({where}{read})")
        print("    Defining one of these as a function is not a link error: the")
        print("    caller loads its first eight bytes of code and jumps to them.")
    print()
    if args.all:
        for group in ("implemented", "stubbed", "missing"):
            if a[group]:
                print(f"  --- {group} ---")
                for s in sorted(a[group]):
                    print(f"    {s}")
                print()
    elif a["missing"]:
        for s in sorted(a["missing"]):
            print(f"    {s}")
        print()


# ---------------------------------------------------------------- stubs --

STUB_HEADER = """\
/* Generated by ldk. Do not edit -- `ldk stubs {name}` rewrites this file.
 *
 * One stub for every symbol {name} asks the kernel for and nk does not yet
 * provide. Each one panics rather than returning a plausible value, because
 * a stub that quietly returns 0 is a driver that appears to work and does
 * something subtly wrong instead -- which is far harder to find than a halt
 * naming the function.
 *
 * This file is committed on purpose. It is the record of what the shim owes
 * this driver, and its diff is the clearest possible statement of what a new
 * driver cost.
 *
 * Implement one by writing it in ../emul/ and re-running `ldk stubs {name}`;
 * anything the shim defines drops out of here automatically.
 */

void nk_stub_called(const char *name);

"""

STUB_ONE = """\
/* @stub {sym} */
long {sym}(void);
long {sym}(void) {{ nk_stub_called("{sym}"); return 0; }}
"""

# A data symbol, which the relocations say is read from rather than called.
# Emitted as a pointer to a panicking function rather than as zero: almost
# every such symbol in a kernel is a function pointer, and this way calling
# through it names the symbol instead of faulting at address zero. For a
# symbol that really is plain data the value is meaningless but harmless --
# and it is still not a jump into the middle of some unrelated function,
# which is what defining it as a function would give.
STUB_DATA = """\
/* @stub {sym} */
static long stub_{sym}(void) {{ nk_stub_called("{sym}"); return 0; }}
void *{sym} = (void *)stub_{sym};
"""


def cmd_stubs(args):
    port = load_port(args.port)
    a = analyse(port)
    STUBS.mkdir(parents=True, exist_ok=True)
    path = STUBS / f"{port['name']}.c"

    want = sorted(a["missing"] | a["stubbed"])
    body = STUB_HEADER.format(name=port["name"])
    body += "\n".join(
        (STUB_DATA if s in a["data"] else STUB_ONE).format(sym=s) for s in want
    )
    path.write_text(body)

    say(f"{len(want)} stubs -> {path.relative_to(ROOT)}")
    if a["implemented"]:
        say(f"{len(a['implemented'])} symbols the shim already provides were left out")
    print()
    print("  Every one of these panics when called. Boot the driver and")
    print("  implement whichever it actually reaches -- which is far fewer")
    print("  than are declared here, and is the whole point.")


# ----------------------------------------------------------------- shim --

# kbuild writes the exact command it used beside every object it builds. Taking
# the flags from there rather than writing them out here means the shim is
# compiled *identically* to the driver it has to link with -- same struct
# layouts, same calling convention, same everything -- and that they cannot
# drift apart later when the kernel version changes.
FLAG_SOURCE = "drivers/virtio/.virtio_mmio.o.cmd"


def kbuild_flags(version: str) -> list[str]:
    """The exact compiler line kbuild used, with the input and output removed.

    The recorded line ends in `-c <source> -o <object>`; those are the two
    things that differ per file. Everything before them -- and there are
    around a hundred flags, several of which change struct layouts -- is what
    has to be identical between the shim and the driver it links with.
    """
    out = docker(f"cat /src/build-arm64/{FLAG_SOURCE}", capture=True)
    line = out.stdout.split("\n", 1)[0]
    # kbuild writes `savedcmd_<path> := gcc ...`; take everything after the
    # first assignment. Splitting on the first '=' alone would cut a flag such
    # as -DKASAN_SHADOW_SCALE_SHIFT= in half.
    for sep in (" := ", " = "):
        if sep in line:
            line = line.split(sep, 1)[1]
            break
    words = shlex.split(line)
    flags, skip = [], False
    for i, w in enumerate(words):
        if skip:
            skip = False
            continue
        if w in ("-o", "-c"):
            skip = w == "-o"
            continue
        if w.endswith(".c") and i > 0:
            continue
        flags.append(w)
    return flags


def cmd_shim(args):
    """Compile the shim against Linux's own headers and archive everything.

    Compiling the shim with Linux's headers in scope is not incidental. It
    means `struct request`, `struct virtio_device` and every other layout is
    the driver's own, byte for byte -- and, just as valuable, that the
    compiler checks each function we write against Linux's own declaration of
    it. A shim function with the wrong signature is a compile error here
    rather than a corrupted stack three stages later.
    """
    port = load_port(args.port)
    out = BUILD / port["name"]
    if not list(out.glob("*.o")):
        die(f"nothing built for {port['name']} -- run: ldk build {port['name']}")

    emul = sorted(EMUL.glob("*.c"))
    stub = STUBS / f"{port['name']}.c"
    if not stub.exists():
        die(f"no stubs for {port['name']} -- run: ldk stubs {port['name']}")

    flags = " ".join(shlex.quote(f) for f in kbuild_flags(port.get("linux", DEFAULT_VERSION)))

    # emul/ first, and on its own, because the stub file cannot be generated
    # correctly until we know what emul defines -- otherwise every function
    # the shim already implements gets a stub too, and the two collide at link
    # time as a duplicate symbol. Ordering this inside one command is the only
    # way the two stay consistent without anyone having to remember.
    shim_out = BUILD / "_shim"
    shim_out.mkdir(parents=True, exist_ok=True)
    say(f"Compiling {len(emul)} shim files with kbuild's own flags")
    _compile(flags, [f"/shim/emul/{p.name}" for p in emul], shim_out)

    say("Regenerating stubs against what the shim now implements")
    cmd_stubs(argparse.Namespace(port=args.port))

    say(f"Compiling {stub.name}")
    _compile(flags, [f"/shim/stubs/{stub.name}"], out / "stub")

    say("Archiving drivers, shim and stubs")
    script = """
set -e
cd /out
rm -f libnklinux.a
ar rcs libnklinux.a *.o stub/*.o /shimobj/*.o
echo "  $(ar t libnklinux.a | wc -l | tr -d ' ') members"
"""
    docker(script, mounts=[(str(out), "/out"), (str(shim_out), "/shimobj")])
    say(f"{(out / 'libnklinux.a').relative_to(ROOT)}")
    others = [p.name for p in BUILD.iterdir()
              if p.is_dir() and p.name not in ("_shim", port["name"])
              and (p / "libnklinux.a").exists()]
    if others:
        # emul/ is shared, so building it for one port leaves every other
        # port's archive holding the previous version of it. Said rather than
        # silently left, because the symptom is a port that was working a
        # minute ago failing in a way that has nothing to do with what changed.
        say(f"other ports now stale: {', '.join(others)}  (ldk shim <port> to refresh)")


def _compile(flags: str, sources: list[str], out: Path):
    out.mkdir(parents=True, exist_ok=True)
    script = f"""
set -e
cd /src/build-arm64
rm -f /out/*.o
fail=0
for src in {' '.join(sources)}; do
  base=$(basename "$src" .c)
  if {flags} -c "$src" -o "/out/$base.o"; then
    echo "  ok    $base.c"
  else
    echo "  FAIL  $base.c"; fail=1
  fi
done
[ "$fail" = 0 ]
"""
    docker(script, mounts=[(str(out), "/out"), (str(KERNEL / "linux"), "/shim")])


# ------------------------------------------------------------------ lkl --

def cmd_lkl(args):
    """Fetch LKL and build it into a single relocatable object.

    LKL is `arch/lkl` in the Linux tree: a real, maintained architecture port
    whose "machine" is a set of function pointers the host fills in. It is
    5,168 lines -- against 26,636 for arch/um and 179,127 for arch/arm64 --
    because it delegates rather than implements.

    The output is `lkl.o`: the entire Linux kernel, for aarch64, as one
    object. Its undefined symbols are the whole of what nk must supply.
    """
    say("Fetching lkl/linux (shallow; a few minutes the first time)")
    subprocess.run(["docker", "volume", "create", LKL_VOLUME],
                   check=True, stdout=subprocess.DEVNULL)
    script = """
set -e
command -v git >/dev/null || { apt-get update -qq && apt-get install -y -qq git; } >/dev/null 2>&1
cd /src
[ -d linux ] || git clone --depth 1 https://github.com/lkl/linux.git
cd linux

# Everything nk needs changed in arch/lkl, with the reasons. Kept in its own
# file because it is real code with real explanations, and because escaping a
# patch through a shell script inside a Python string is a way to spend an
# afternoon on backslashes.
python3 /shim/ldk/patch-lkl.py /shim

if [ ! -f .config ]; then
  make ARCH=lkl defconfig >/dev/null
  # The ordinary console registers at core_initcall, which is a long way into
  # start_kernel -- so a boot that fails before it produces no output at all,
  # which is indistinguishable from one that never started. The early console
  # registers at early_initcall and prints the whole log.
  echo "CONFIG_LKL_EARLY_CONSOLE=y" >> .config
  make ARCH=lkl olddefconfig >/dev/null
fi
# -mno-outline-atomics: without it gcc emits calls to __aarch64_*_sync
# helpers that live in libgcc, and nk has no libgcc. Inline atomics are
# what a kernel wants anyway -- an out-of-line call per atomic on a
# machine that has LSE instructions is a strange thing to pay for.
make ARCH=lkl KCFLAGS="-mno-outline-atomics" -j$(nproc) >/tmp/build.log 2>&1 \
    || { tail -30 /tmp/build.log; exit 1; }
cp arch/lkl/include/uapi/asm/host_ops.h /out/
cp arch/lkl/include/uapi/lkl.h /out/ 2>/dev/null || true
# Stripped of debug info: 344MB to 20MB, and nk links the whole thing.
strip --strip-debug lkl.o -o /out/lkl.o
gcc -c -ffreestanding -fno-stack-protector -fno-PIE -mgeneral-regs-only \
    -Wall -Wextra -Wno-unused-parameter -I/out /shim/lkl/nk-host.c -o /out/nk-host.o
rm -f /out/libnklkl.a && ar rcs /out/libnklkl.a /out/lkl.o /out/nk-host.o
echo "  lkl.o: $(stat -c%s /out/lkl.o) bytes stripped"
echo "  the whole Linux kernel still wants, from nk:"
ld -r -o /tmp/c.o /out/lkl.o /out/nk-host.o
nm --undefined-only /tmp/c.o | awk '{print "    " $2}' | sort -u
"""
    out = BUILD / "lkl"
    out.mkdir(parents=True, exist_ok=True)
    cmd = ["docker", "run", "--rm", "-v", f"{LKL_VOLUME}:/src",
           "-v", f"{out}:/out", "-v", f"{KERNEL}:/shim", IMAGE, "sh", "-c", script]
    subprocess.run(cmd, check=True)
    say(f"{(out / 'lkl.o').relative_to(ROOT)}")
    print()
    print("  That object is a complete Linux kernel: VFS, ext4, the network")
    print("  stack, and every system call. What it wants from nk is the list")
    print("  above plus a struct lkl_host_operations -- see kernel/lkl/.")


# --------------------------------------------------------------- report --

def cmd_report(args):
    print()
    print(f"  {'port':<14} {'objects':>7} {'needs':>7} {'done':>6} {'stub':>6} {'todo':>6}")
    print(f"  {'-'*14} {'-'*7:>7} {'-'*7:>7} {'-'*6:>6} {'-'*6:>6} {'-'*6:>6}")
    shim = shim_symbols()
    seen: set[str] = set()
    for path in sorted(PORTS.glob("*.json")):
        port = json.loads(path.read_text())
        out = BUILD / port["name"]
        if not list(out.glob("*.o")):
            print(f"  {port['name']:<14} {'—':>7} {'not built':>29}")
            continue
        a = analyse(port)
        print(f"  {port['name']:<14} {len(a['objects']):>7} {len(a['external']):>7} "
              f"{len(a['implemented']):>6} {len(a['stubbed']):>6} {len(a['missing']):>6}")
        # What this port asks for that no earlier port did. The number that
        # actually decides whether another driver class is worth attempting:
        # the second driver of a kind is cheap, the first of a kind is not.
        new = a["external"] - seen
        if seen:
            print(f"  {'':<14} {'':>7} {len(new):>7} new beyond the ports above")
        seen |= a["external"]
    print()
    print(f"  shim implements {len(shim)} symbols")
    print()


def cmd_ports(args):
    for path in sorted(PORTS.glob("*.json")):
        port = json.loads(path.read_text())
        built = "built" if list((BUILD / port["name"]).glob("*.o")) else "not built"
        print(f"  {port['name']:<14} {port['summary']}  ({built})")


def main():
    ap = argparse.ArgumentParser(
        prog="ldk", description=__doc__.split("\n\n")[1],
        formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("fetch", help="a Linux tree, configured for arm64")
    p.add_argument("--version", default=DEFAULT_VERSION)
    p.set_defaults(func=cmd_fetch)

    p = sub.add_parser("build", help="compile a port's drivers, unmodified")
    p.add_argument("port")
    p.set_defaults(func=cmd_build)

    p = sub.add_parser("syms", help="what a port needs that nk does not provide")
    p.add_argument("port")
    p.add_argument("--all", action="store_true", help="list every group, not only what is missing")
    p.set_defaults(func=cmd_syms)

    p = sub.add_parser("stubs", help="write a panicking stub for each missing symbol")
    p.add_argument("port")
    p.set_defaults(func=cmd_stubs)

    p = sub.add_parser("shim", help="compile the shim and archive it with the drivers")
    p.add_argument("port")
    p.set_defaults(func=cmd_shim)

    p = sub.add_parser("lkl", help="fetch and build LKL: the whole Linux kernel as one object")
    p.set_defaults(func=cmd_lkl)

    p = sub.add_parser("report", help="coverage across every port")
    p.set_defaults(func=cmd_report)

    p = sub.add_parser("ports", help="what ports exist")
    p.set_defaults(func=cmd_ports)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
