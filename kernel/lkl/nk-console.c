// SPDX-License-Identifier: GPL-2.0
/*
 * A console Linux owns, backed by nk's UART.
 *
 * Before this, nk answered writes to descriptors 1 and 2 itself: it looked at
 * the number, and if it was 1 or 2 the bytes went to the PL011 without Linux
 * being told. That works for a program that only prints, and it is exactly
 * wrong for a shell, because `ls > file` is a dup2 of a *file* onto descriptor
 * 1 and nk would have gone on writing to the UART. Descriptors have to mean
 * what Linux says they mean, which means the console has to be a file.
 *
 * So this is a real tty driver, and a small one, because everything hard about
 * a tty -- line discipline, canonical mode, echo, job control -- is Linux's
 * and already written. What is left is a way out (`lkl_ops->print`, which is
 * the host operation LKL already uses for printk) and a way in (an interrupt
 * the host raises when a key arrives, and a host call to collect it).
 *
 * /dev/console reaches this through `struct console.device`: Linux's
 * `console_device()` walks the registered consoles and asks each for the tty
 * driver behind it. LKL's own console has no `.device`, so this one answers.
 */
#include <linux/init.h>
#include <linux/interrupt.h>
#include <linux/console.h>
#include <linux/tty.h>
#include <linux/tty_driver.h>
#include <linux/tty_flip.h>
#include <asm/host_ops.h>
#include <asm/irq.h>
#include <uapi/asm/irq.h>

/*
 * Provided by nk. Drains whatever the host has buffered and returns how many
 * bytes it wrote; zero when there is nothing. It touches only the host's own
 * ring, which is why it is safe to call from interrupt context here.
 */
int nk_console_read(char *buf, int max);

/*
 * Told to nk once, at init, rather than asked for.
 *
 * The other direction does not survive: a global function that nothing inside
 * the kernel calls is dropped by the kernel's own --gc-sections long before
 * nk's reference to it is resolved, and the link fails with an undefined
 * symbol whose definition is plainly in the source. An initcall is always
 * kept, so pushing the number out of one is the arrangement that works.
 */
void nk_console_ready(int irq);

static struct tty_driver *nk_tty_driver;
static struct tty_port nk_tty_port;
static int nk_console_irq_no = -1;

static irqreturn_t nk_console_isr(int irq, void *dev)
{
	char buf[128];
	int n;

	while ((n = nk_console_read(buf, sizeof(buf))) > 0) {
		tty_insert_flip_string(&nk_tty_port, buf, n);
		tty_flip_buffer_push(&nk_tty_port);
	}
	return IRQ_HANDLED;
}

static int nk_tty_open(struct tty_struct *tty, struct file *filp)
{
	return tty_port_open(&nk_tty_port, tty, filp);
}

static void nk_tty_close(struct tty_struct *tty, struct file *filp)
{
	tty_port_close(&nk_tty_port, tty, filp);
}

static ssize_t nk_tty_write(struct tty_struct *tty, const u8 *buf, size_t count)
{
	if (lkl_ops->print)
		lkl_ops->print((const char *)buf, count);
	return count;
}

/* Unbounded, because the host's write does not block: it is a UART with a
 * bounded spin and no queue, so there is never a reason to make Linux wait. */
static unsigned int nk_tty_write_room(struct tty_struct *tty)
{
	return 4096;
}

static const struct tty_operations nk_tty_ops = {
	.open		= nk_tty_open,
	.close		= nk_tty_close,
	.write		= nk_tty_write,
	.write_room	= nk_tty_write_room,
};

static const struct tty_port_operations nk_tty_port_ops = { };

static struct tty_driver *nk_console_device(struct console *co, int *index)
{
	*index = 0;
	return nk_tty_driver;
}

static void nk_console_write(struct console *co, const char *s, unsigned int n)
{
	if (lkl_ops->print)
		lkl_ops->print(s, n);
}

static struct console nk_console = {
	.name	= "ttyNK",
	.write	= nk_console_write,
	.device	= nk_console_device,
	.flags	= CON_PRINTBUFFER,
	.index	= -1,
};

static int __init nk_console_init(void)
{
	int ret;

	nk_tty_driver = tty_alloc_driver(1, TTY_DRIVER_REAL_RAW |
					    TTY_DRIVER_DYNAMIC_DEV);
	if (IS_ERR(nk_tty_driver))
		return PTR_ERR(nk_tty_driver);

	nk_tty_driver->driver_name = "nk_console";
	nk_tty_driver->name = "ttyNK";
	nk_tty_driver->major = TTY_MAJOR;
	nk_tty_driver->minor_start = 64;
	nk_tty_driver->type = TTY_DRIVER_TYPE_SERIAL;
	nk_tty_driver->subtype = SERIAL_TYPE_NORMAL;
	nk_tty_driver->init_termios = tty_std_termios;
	tty_set_operations(nk_tty_driver, &nk_tty_ops);

	tty_port_init(&nk_tty_port);
	nk_tty_port.ops = &nk_tty_port_ops;
	tty_port_link_device(&nk_tty_port, nk_tty_driver, 0);

	ret = tty_register_driver(nk_tty_driver);
	if (ret) {
		tty_driver_kref_put(nk_tty_driver);
		return ret;
	}
	tty_register_device(nk_tty_driver, 0, NULL);

	nk_console_irq_no = lkl_get_free_irq("nk-console");
	if (nk_console_irq_no >= 0 &&
	    request_irq(nk_console_irq_no, nk_console_isr, 0, "nk-console", NULL))
		nk_console_irq_no = -1;

	register_console(&nk_console);
	/* Now the host can raise it. */
	nk_console_ready(nk_console_irq_no);
	return 0;
}
device_initcall(nk_console_init);
