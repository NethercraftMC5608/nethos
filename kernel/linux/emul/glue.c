// SPDX-License-Identifier: GPL-2.0
/*
 * Where Linux code meets nk.
 *
 * Every file in this directory is compiled with Linux's own headers in scope
 * and with the exact flags kbuild used for the driver beside it -- see `ldk
 * shim`. That is not a convenience. It means `struct request`, `struct
 * virtio_device` and every other layout here is the driver's own, byte for
 * byte, and it means the compiler checks each function we write against
 * Linux's own declaration of it. A shim function with the wrong signature is
 * a compile error in this file rather than a corrupted stack three stages
 * later.
 *
 * The other direction -- calling *into* nk -- goes through the small nk_*
 * interface declared here, and nothing else. The Rust side knows nothing
 * about Linux's types and must not start to.
 */

#include <linux/init.h>
#include <linux/kernel.h>
#include <linux/printk.h>

/* Implemented in Rust; see kernel/core/src/linux.rs. */
void nk_console_write(const char *s, unsigned long len);
void nk_halt(void);

/*
 * -fstack-protector-strong is in the flags kbuild gave us, so every non-
 * trivial function reads this and calls the failure handler. Linux allocates
 * a real per-task canary; nk has one task's worth of ambition here and a
 * fixed value, which still catches a linear overflow -- the thing the canary
 * is actually for -- and costs nothing.
 *
 * ldk deliberately does not stub these: they are the toolchain's, not the
 * kernel's, and a generated stub for __stack_chk_fail that returned would
 * defeat the check entirely.
 */
unsigned long __stack_chk_guard = 0x000a0dff5f5f5f5fUL;

void __stack_chk_fail(void);
void __stack_chk_fail(void)
{
	nk_console_write("\n!! stack smashing detected in Linux code\n", 41);
	nk_halt();
}

/*
 * Run every initcall the linked-in drivers registered.
 *
 * `module_init` on a built-in driver is not a constructor -- it is an entry
 * in a `.initcallN.init` section, and linker.ld gathers those in level order
 * so that, for example, the virtio bus (level 4) registers before the drivers
 * that want to attach to it (level 6).
 *
 * The walk is written here rather than in Rust for one specific reason: on
 * arm64 `CONFIG_HAVE_ARCH_PREL32_RELOCATIONS` is set, so an initcall entry is
 * a 32-bit *relative offset*, not a pointer. Reimplementing that in Rust
 * would be a detail to get wrong; here, `initcall_from_entry` is Linux's own
 * and cannot be.
 */
extern initcall_entry_t __initcall_start[];
extern initcall_entry_t __initcall_end[];

int nk_linux_init(void);
int nk_linux_init(void)
{
	initcall_entry_t *fn;
	int ran = 0;

	/*
	 * A format check before anything depends on the log being readable.
	 * Cheap, once, and it earns its place: an early boot where %u printed
	 * as '?' made every driver message useless at exactly the point they
	 * were the only diagnostic available.
	 */
	pr_info("printk check: u=%u d=%d x=%#x s=%s p=%p\n",
		42u, -7, 0xabcd, "ok", (void *)0x1234);

	for (fn = __initcall_start; fn < __initcall_end; fn++) {
		initcall_t call = initcall_from_entry(fn);
		int ret = call();

		ran++;
		if (ret)
			pr_warn("initcall %pS returned %d\n", call, ret);
	}
	return ran;
}
