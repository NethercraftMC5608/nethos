//! What a generated stub calls when a driver reaches something the shim does
//! not implement yet.
//!
//! It halts, loudly, naming the function. It does not return a plausible
//! value, and that is the single most important decision in the whole shim:
//! a stub that quietly returns 0 gives a driver that appears to work and does
//! something subtly wrong instead, which is far harder to find than a stop
//! with a name attached. The generated stubs return `long` only because C
//! requires a type; the return never happens.
//!
//! This is also the mechanism that makes "the shim is discovered, not
//! designed" a workable method rather than a slogan. Every stub is a
//! measurement: boot the driver, read the name it stopped on, implement that
//! one, boot again. `ldk` declares hundreds; a first driver reaches a small
//! fraction of them.

use crate::println;

/// # Safety
/// Called from generated C with a static, NUL-terminated string.
#[no_mangle]
pub extern "C" fn nk_stub_called(name: *const u8) -> ! {
    println!();
    println!("!! unimplemented Linux API");
    if name.is_null() {
        println!("   (the stub passed no name -- ldk generated it wrong)");
    } else {
        // No CStr: this is the panic path and it must not allocate or rely on
        // anything the missing function might have been part of.
        crate::print!("   ");
        let mut p = name;
        unsafe {
            let mut n = 0;
            while *p != 0 && n < 256 {
                crate::uart::console().put(*p);
                p = p.add(1);
                n += 1;
            }
        }
        println!();
    }
    println!();
    println!("   Implement it in kernel/linux/emul/, then: ldk stubs <port>");
    crate::halt();
}
