// SPDX-License-Identifier: GPL-2.0
/*
 * Workqueues: deferred work, run on a kernel thread.
 *
 * A real one, not a stub that runs the work inline. Running work immediately
 * would be the obvious shortcut and it is wrong in a way that is very hard to
 * find: work is queued precisely *because* the caller is somewhere it must
 * not do this -- inside an interrupt handler, or holding a lock the work will
 * take. Running it there deadlocks or corrupts, at a point far from the
 * queue_work that caused it.
 *
 * So there is one worker thread, and queue_work puts an item on a list it
 * drains. One thread for every workqueue in the system, rather than one each:
 * nk's scheduler has sixteen task slots and no concurrency to exploit, and a
 * thread per queue would spend them on nothing.
 */

#include <linux/workqueue.h>

#include "nk.h"

#define MAX_PENDING 64

static struct work_struct *pending[MAX_PENDING];
static int head, tail;

struct workqueue_struct {
	const char *name;
};

static struct workqueue_struct queues[8];
static int nqueues;

struct workqueue_struct *alloc_workqueue_noprof(const char *fmt, unsigned int flags,
						int max_active, ...)
{
	struct workqueue_struct *wq;

	(void)flags;
	(void)max_active;
	if (nqueues == ARRAY_SIZE(queues))
		return NULL;
	wq = &queues[nqueues++];
	wq->name = fmt;
	return wq;
}

void destroy_workqueue(struct workqueue_struct *wq)
{
	(void)wq;
}

bool queue_work_on(int cpu, struct workqueue_struct *wq, struct work_struct *work)
{
	unsigned long long flags;
	bool queued = false;

	(void)cpu;
	(void)wq;
	/*
	 * Masked, not locked: this is called from interrupt context, and the
	 * worker thread drains the same array from thread context.
	 */
	flags = nk_irq_save();
	if ((head + 1) % MAX_PENDING != tail) {
		pending[head] = work;
		head = (head + 1) % MAX_PENDING;
		queued = true;
	}
	nk_irq_restore(flags);
	return queued;
}

bool flush_work(struct work_struct *work)
{
	/*
	 * Drain everything rather than track this one item. The queue is
	 * short and flush_work is not on any hot path; a per-item completion
	 * would be more code to be wrong in.
	 */
	(void)work;
	while (head != tail)
		nk_yield();
	return true;
}

/* The worker thread's body, run by nk. */
void nk_work_drain(void);
void nk_work_drain(void)
{
	while (head != tail) {
		unsigned long long flags = nk_irq_save();
		struct work_struct *work = pending[tail];

		tail = (tail + 1) % MAX_PENDING;
		nk_irq_restore(flags);

		if (work && work->func)
			work->func(work);
	}
}
