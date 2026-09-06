// The linker script is passed as an absolute path rather than through
// .cargo/config.toml, because rustflags there are resolved against whatever
// directory cargo happens to be invoked from -- which is the workspace root
// for `cargo build` and the package root for `cargo build -p`, so a relative
// -T path works from one and not the other.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=src/boot.s");

    // Keep nk_stub_called even though nothing in the kernel calls it.
    //
    // It exists for the generated stubs in kernel/linux/stubs/, which are C
    // and are not linked in until Stage 3. Until then nothing references it,
    // the linker garbage-collects it, and it is simply absent from the
    // binary -- which would present at Stage 3 as every stub failing to link
    // against a function that is plainly right there in the source.
    println!("cargo:rustc-link-arg=--undefined=nk_stub_called");

    // The Linux side, when there is one. `ldk shim <port>` produces the
    // archive; NK_LINUX_LIB names it. Absent, nk builds exactly as it did
    // before Stage 3 -- which keeps the kernel buildable and testable on a
    // machine with no docker and no Linux tree.
    println!("cargo:rerun-if-env-changed=NK_LINUX_LIB");
    // Declared unconditionally so cargo does not warn about an unknown cfg on
    // the builds where it is absent -- which is most of them.
    println!("cargo::rustc-check-cfg=cfg(nk_linux)");
    if let Ok(lib) = std::env::var("NK_LINUX_LIB") {
        // Canonicalised, not used as given. cargo runs build scripts with the
        // package directory as cwd, so a relative NK_LINUX_LIB resolves
        // against kernel/core/ rather than wherever it was typed -- and the
        // error for that is "unable to find library -lnklinux", which names
        // neither the path nor the reason.
        let path = std::fs::canonicalize(&lib)
            .unwrap_or_else(|e| panic!("NK_LINUX_LIB={lib}: {e}"));
        let dir = path.parent().unwrap().display();
        println!("cargo:rustc-link-search=native={dir}");
        // Passed as link args rather than through rustc-link-lib, which would
        // add a *second*, non-whole copy of the archive and make every symbol
        // in it a duplicate of itself.
        //
        // Everything Linux exports is unreferenced from nk's point of view --
        // the drivers are reached through initcall tables, not by name -- so
        // without this the linker takes none of the archive at all.
        println!("cargo:rustc-link-arg=--whole-archive");
        println!("cargo:rustc-link-arg=-lnklinux");
        println!("cargo:rustc-link-arg=--no-whole-archive");
        println!("cargo:rustc-link-arg=--undefined=nk_linux_init");
        // Everything that talks to Linux is compiled out without this, so nk
        // still builds and boots on a machine with no docker and no Linux
        // tree -- and so a broken shim cannot stop the kernel being tested.
        println!("cargo:rustc-cfg=nk_linux");
        println!("cargo:rerun-if-changed={lib}");
    }
}
