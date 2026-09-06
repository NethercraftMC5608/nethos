// SPDX-License-Identifier: GPL-2.0
/*
 * Device interrupts: registration, and the dispatch from nk's IRQ path.
 *
 * The interrupt number a driver asks for is the GIC's INTID, unchanged. Linux
 * puts a whole layer here -- irq domains mapping a controller's hardware
 * numbers onto a flat virtual space, so that several controllers can coexist
 * without colliding. nk has one interrupt controller and takes its numbers
 * literally, which is correct until it has two.
 *
 * Threaded handlers are not threaded. request_threaded_irq's contract is that
 * the hard handler runs in interrupt context and returns IRQ_WAKE_THREAD to
 * defer the rest; virtio's handler never does -- it completes in the hard
 * handler and returns IRQ_HANDLED. The thread function is stored and its
 * absence is reported rather than silently ignored, because a driver that
 * relies on it would otherwise appear to work and simply never finish
 * anything.
 */

#include <linux/interrupt.h>
#include <linux/irq.h>
#include <linux/slab.h>

#include "nk.h"

#define MAX_HANDLERS 32

struct entry {
	unsigned int irq;
	irq_handler_t handler;
	irq_handler_t thread_fn;
	void *dev_id;
	const char *name;
};

static struct entry handlers[MAX_HANDLERS];
static int count;

int request_threaded_irq(unsigned int irq, irq_handler_t handler,
			 irq_handler_t thread_fn, unsigned long flags,
			 const char *name, void *dev)
{
	struct entry *e;

	(void)flags;
	if (count == MAX_HANDLERS)
		return -ENOSPC;
	/*
	 * Shared interrupts work by chaining: several entries with the same
	 * number, each called in turn until one claims it. That falls out of
	 * the table without a special case, which is why there is none.
	 */
	e = &handlers[count++];
	e->irq = irq;
	e->handler = handler;
	e->thread_fn = thread_fn;
	e->dev_id = dev;
	e->name = name;

	return nk_request_irq(irq);
}

void free_irq_ret(unsigned int irq, void *dev_id, void **ret);
void free_irq_ret(unsigned int irq, void *dev_id, void **ret)
{
	int i;

	for (i = 0; i < count; i++) {
		if (handlers[i].irq == irq && handlers[i].dev_id == dev_id) {
			handlers[i].handler = NULL;
			break;
		}
	}
	if (ret)
		*ret = dev_id;
}

const void *free_irq(unsigned int irq, void *dev_id)
{
	void *ret = NULL;

	free_irq_ret(irq, dev_id, &ret);
	return ret;
}

/* One CPU, and this is only ever called from thread context with the handler
 * not running. There is nothing to wait for. */
void synchronize_irq(unsigned int irq)
{
	(void)irq;
}

int irq_set_irq_wake(unsigned int irq, unsigned int on)
{
	/* nk does not suspend, so nothing can be a wake source. Reporting
	 * success would be a claim; -ENXIO is the truth and every caller of
	 * this treats it as advisory. */
	(void)irq;
	(void)on;
	return -ENXIO;
}

/*
 * Called from nk's IRQ path for every interrupt that is not nk's own timer.
 * Returns non-zero if somebody claimed it.
 */
int nk_linux_irq(unsigned int intid);
int nk_linux_irq(unsigned int intid)
{
	int i, claimed = 0;

	for (i = 0; i < count; i++) {
		struct entry *e = &handlers[i];

		if (e->irq != intid || !e->handler)
			continue;
		if (e->handler(intid, e->dev_id) == IRQ_WAKE_THREAD) {
			if (e->thread_fn)
				e->thread_fn(intid, e->dev_id);
			else
				pr_warn("irq %u wanted a thread and has none\n",
					intid);
		}
		claimed = 1;
	}
	return claimed;
}
