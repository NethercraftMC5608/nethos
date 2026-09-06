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
}
