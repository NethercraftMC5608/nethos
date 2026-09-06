// SPDX-License-Identifier: GPL-2.0
/*
 * The odds and ends: things a driver touches once, that belong to no
 * subsystem nk has, and that are each a few lines.
 *
 * Grouped in one file on purpose. Splitting them into an ida.c, an rng.c, a
 * sysfs.c and so on would give eight files of nine lines with a subsystem's
 * name on each, and imply nk has eight subsystems it does not have. Anything
 * here that grows should leave and take its own file with it.
 */

#include <linux/device.h>
#include <linux/idr.h>
#include <linux/kernel.h>
#include <linux/kobject.h>
#include <linux/moduleparam.h>
#include <linux/notifier.h>
#include <linux/random.h>
#include <linux/rcupdate.h>
#include <linux/string.h>
#include <linux/string_helpers.h>
#include <linux/sysfs.h>
#include <linux/virtio.h>
#include <xen/xen.h>

#include "nk.h"

/*
 * Linux defers a caller until the random pool is seeded, so that a driver
 * cannot bake an unseeded value into something permanent. nk has no entropy
 * source, so there is nothing to wait for and nothing to defer -- the
 * notifier is called immediately.
 *
 * This is a real weakening, and it is the reason it is written here rather
 * than stubbed: virtio_blk uses it to pick a disk's name. Anything that ever
 * needs randomness for a *security* decision must not use this, and there is
 * currently no way for it to tell.
 */
int execute_with_initialized_rng(struct notifier_block *nb)
{
	return nb->notifier_call(nb, 0, NULL);
}

/*
 * ID allocation. A bitmap would be the honest general answer; a counter is
 * the honest answer for nk, where the only caller asks for one disk index per
 * device at probe and nothing is ever removed. It is wrong the first time
 * something frees an ID and expects to get it back, and it says so.
 */
static unsigned int next_id;

int ida_alloc_range(struct ida *ida, unsigned int min, unsigned int max,
		    gfp_t gfp)
{
	unsigned int id;

	(void)ida;
	(void)gfp;
	id = next_id++;
	if (id < min)
		id = min, next_id = min + 1;
	if (id > max)
		return -ENOSPC;
	return id;
}

void ida_free(struct ida *ida, unsigned int id)
{
	/* Never reused. See above. */
	(void)ida;
	(void)id;
}

void ida_destroy(struct ida *ida)
{
	(void)ida;
}

/*
 * sysfs. nk has no filesystem of any kind, so nothing here can be read by
 * anybody -- but a driver's attribute callbacks still run and still format
 * into a buffer, and returning a wrong length would corrupt whatever the
 * caller does next. So the formatting is real and only the publishing is
 * absent.
 */
int sysfs_emit(char *buf, const char *fmt, ...)
{
	va_list args;
	int n;

	va_start(args, fmt);
	n = vscnprintf(buf, PAGE_SIZE, fmt, args);
	va_end(args);
	return n;
}

int sysfs_emit_at(char *buf, int at, const char *fmt, ...)
{
	va_list args;
	int n;

	if (at < 0 || at >= (int)PAGE_SIZE)
		return 0;
	va_start(args, fmt);
	n = vscnprintf(buf + at, PAGE_SIZE - at, fmt, args);
	va_end(args);
	return n;
}

int __sysfs_match_string(const char * const *array, size_t n, const char *str)
{
	size_t i;

	for (i = 0; i < n; i++) {
		if (!array[i])
			break;
		if (strcmp(array[i], str) == 0)
			return i;
	}
	return -EINVAL;
}

int add_uevent_var(struct kobj_uevent_env *env, const char *format, ...)
{
	/* Uevents go to userspace, and nk has none. */
	(void)env;
	(void)format;
	return 0;
}

/*
 * RCU with one CPU and no preemption inside a read-side critical section is
 * trivially satisfied: by the time a writer runs, every reader has finished,
 * because there is nobody else to be mid-read.
 *
 * This stops being true the moment nk preempts inside an rcu_read_lock, which
 * it currently cannot -- the scheduler only switches from the timer handler,
 * and RCU readers here never sleep. It is a real assumption, not a no-op.
 */
void synchronize_rcu(void)
{
}

/* nk is not a Xen guest, and this is how virtio_ring asks. */
enum xen_domain_type xen_domain_type = XEN_NATIVE;

/*
 * Module parameters have no one to set them: nk has no command line yet, so
 * every parameter keeps its compiled-in default. The ops table has to exist
 * because the drivers' __param sections point at it.
 */
const struct kernel_param_ops param_ops_uint = {
	.set = NULL,
	.get = NULL,
};

/*
 * "512 GB" and the like, for a capacity line in the log. Linux's version
 * handles SI and IEC units and rounding rules across several helpers; this
 * one is only ever read by a human looking at a boot message.
 */
int string_get_size(u64 size, u64 blk_size, const enum string_size_units units,
		    char *buf, int len)
{
	static const char *const suffix[] = { "B", "KiB", "MiB", "GiB", "TiB" };
	u64 total = size * blk_size;
	unsigned int i = 0;

	(void)units;
	while (total >= 1024 && i < ARRAY_SIZE(suffix) - 1) {
		total /= 1024;
		i++;
	}
	return snprintf(buf, len, "%llu %s", total, suffix[i]);
}

/*
 * Whether this system *restricts* what memory a virtio device may reach.
 *
 * Read the name carefully -- it is a question, not a permission, and the
 * polarity is the opposite of what it looks like. Returning true means
 * "restricted access is in force", which makes virtio_features_ok demand both
 * VIRTIO_F_VERSION_1 and VIRTIO_F_ACCESS_PLATFORM of every device and reject
 * any that lacks them. It is for confidential-computing guests and Xen, where
 * the device genuinely cannot reach arbitrary memory.
 *
 * nk has a flat identity map and no restriction of any kind, so the answer is
 * false -- Linux's own default, `virtio_no_restricted_mem_acc`. Answering
 * true instead rejected every device on the bus with "device must provide
 * VIRTIO_F_VERSION_1", which reads like a fault in the device and was a fault
 * in this file.
 *
 * A function *pointer*, not a function, and the difference cost real time.
 * include/linux/virtio_anchor.h declares it as
 *
 *     extern bool (*virtio_check_mem_acc_cb)(struct virtio_device *dev);
 *
 * and no header the shim includes says so, so defining it as a function
 * compiled and linked without complaint. virtio_features_ok then loaded the
 * first eight bytes of this function's machine code and branched to them --
 * `mov w0, #1; ret`, which is what `return true` compiles to. The fault
 * reported an address of 0xd65f03c052800020, which is those two instructions,
 * and nothing in it pointed anywhere near here.
 *
 * `ldk syms` now reports which symbols are read from rather than called, so
 * the next one of these is caught before it runs. See npkg_elf.references.
 */
static bool no_restricted_mem_acc(struct virtio_device *vdev)
{
	(void)vdev;
	return false;
}

bool (*virtio_check_mem_acc_cb)(struct virtio_device *dev) = no_restricted_mem_acc;
