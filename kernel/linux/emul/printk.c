// SPDX-License-Identifier: GPL-2.0
/*
 * printk, and the dev_* family that almost every driver line goes through.
 *
 * The first thing worth implementing rather than stubbing, because it is what
 * every *other* thing being wrong will be reported through. A driver whose
 * printk halts tells you nothing at all.
 *
 * vsnprintf is Linux's own, taken from lib/vsprintf.c, so %pS, %pOF and the
 * rest of the kernel's extended specifiers behave -- a driver that formats a
 * device with %pOF and gets a literal "%pOF" back is a lie in the logs.
 */

#include <linux/device.h>
#include <linux/kernel.h>
#include <linux/printk.h>

void nk_console_write(const char *s, unsigned long len);

static char buf[1024];

static void emit(const char *prefix, const char *fmt, va_list args)
{
	int n;

	if (prefix)
		nk_console_write(prefix, strlen(prefix));
	n = vscnprintf(buf, sizeof(buf), fmt, args);
	if (n > 0)
		nk_console_write(buf, n);
	if (n <= 0 || buf[n - 1] != '\n')
		nk_console_write("\n", 1);
}

/*
 * Linux's own loglevel prefix is "\001" followed by a digit, embedded at the
 * start of the format string by the pr_* macros. Stripped rather than
 * printed: it is a wire format for the ring buffer, not text.
 */
static const char *strip_level(const char *fmt)
{
	if (fmt[0] == KERN_SOH_ASCII && fmt[1])
		return fmt + 2;
	return fmt;
}

int _printk(const char *fmt, ...)
{
	va_list args;

	va_start(args, fmt);
	emit("[linux] ", strip_level(fmt), args);
	va_end(args);
	return 0;
}

/*
 * The dev_* family. Linux's versions print the bus and device name in front
 * of the message; keeping that is worth the three lines, because with several
 * virtio devices on one bus an unattributed message says nothing about which.
 */
static void dev_emit(const char *level, const struct device *dev,
		     const char *fmt, va_list args)
{
	nk_console_write("[linux] ", 8);
	nk_console_write(level, strlen(level));
	if (dev && dev_name(dev)) {
		const char *name = dev_name(dev);

		nk_console_write(name, strlen(name));
		nk_console_write(": ", 2);
	}
	emit(NULL, fmt, args);
}

#define DEV_PRINTER(fn, level)						\
	void fn(const struct device *dev, const char *fmt, ...)		\
	{								\
		va_list args;						\
		va_start(args, fmt);					\
		dev_emit(level, dev, fmt, args);			\
		va_end(args);						\
	}

DEV_PRINTER(_dev_err, "error: ")
DEV_PRINTER(_dev_warn, "warn: ")
DEV_PRINTER(_dev_notice, "")
DEV_PRINTER(_dev_info, "")

void __warn_printk(const char *fmt, ...)
{
	va_list args;

	va_start(args, fmt);
	emit("[linux] WARN: ", fmt, args);
	va_end(args);
}
