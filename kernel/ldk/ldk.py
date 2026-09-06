#!/usr/bin/env python3
"""
ldk — the Linux Driver Kit. What nk owes an unmodified Linux driver.

    ldk fetch                 a Linux tree, configured for arm64
    ldk build   <port>        compile the port's drivers, unmodified
    ldk syms    <port>        what they need that nk does not provide
    ldk stubs   <port>        write a stub for every one of those
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
DEFAULT_VERSION = "7.2"

# Symbols every object references that are the toolchain's business rather
# than the kernel's. Stubbing these would be actively wrong.
TOOLCHAIN = {
    "__stack_chk_guard",
    "__stack_chk_fail",
    "memcpy", "memmove", "memset", "memcmp",
    "strlen", "strcmp", "strncmp", "strcpy", "strncpy",
    "__aeabi_unwind_cpp_pr0", "__aeabi_unwind_cpp_pr1",
}


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
    return subprocess.run(cmd, check=check, text=True,
                          capture_output=capture)


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

def objects_of(port: dict) -> list[str]:
    return [s[:-2] + ".o" for s in port["sources"]]


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

    # A symbol one file in the port defines and another uses is internal to
    # the port and nobody's responsibility but its own.
    external = undefined - defined - TOOLCHAIN
    implemented = external & shim_symbols()
    stubbed = (external - implemented) & stubbed_symbols(port["name"])
    missing = external - implemented - stubbed
    return {
        "objects": [o.name for o in objs],
        "defined": defined,
        "external": external,
        "implemented": implemented,
        "stubbed": stubbed,
        "missing": missing,
    }


def cmd_syms(args):
    port = load_port(args.port)
    a = analyse(port)
    print()
    print(f"  {port['name']} — {port['summary']}")
    print(f"  {len(a['objects'])} objects, {len(a['defined'])} symbols defined")
    print()
    print(f"  needs {len(a['external'])} symbols from the kernel underneath it")
    print(f"    implemented   {len(a['implemented'])}")
    print(f"    stubbed       {len(a['stubbed'])}")
    print(f"    not yet       {len(a['missing'])}")
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


def cmd_stubs(args):
    port = load_port(args.port)
    a = analyse(port)
    STUBS.mkdir(parents=True, exist_ok=True)
    path = STUBS / f"{port['name']}.c"

    want = sorted(a["missing"] | a["stubbed"])
    body = STUB_HEADER.format(name=port["name"])
    body += "\n".join(STUB_ONE.format(sym=s) for s in want)
    path.write_text(body)

    say(f"{len(want)} stubs -> {path.relative_to(ROOT)}")
    if a["implemented"]:
        say(f"{len(a['implemented'])} symbols the shim already provides were left out")
    print()
    print("  Every one of these panics when called. Boot the driver and")
    print("  implement whichever it actually reaches -- which is far fewer")
    print("  than are declared here, and is the whole point.")


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

    p = sub.add_parser("report", help="coverage across every port")
    p.set_defaults(func=cmd_report)

    p = sub.add_parser("ports", help="what ports exist")
    p.set_defaults(func=cmd_ports)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
