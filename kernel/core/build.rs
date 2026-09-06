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
}
