// SPDX-License-Identifier: GPL-2.0
/*
 * Bottom halves, RCU read sections and preemption, on a kernel where all
 * three are much simpler than they are in Linux -- and where saying *why*
 * matters more than the code, because each is a correctness claim.
 *
 * **Bottom-half disabling is interrupt masking.** local_bh_disable stops
 * softirqs running on this CPU while leaving hardware interrupts on. nk runs
 * softirq work on a thread rather than on the interrupt return path, so the
 * thing being excluded is another thread -- and with one CPU and a scheduler
 * that only switches on a tick or a yield, masking interrupts excludes it.
 *
 * **RCU read sections are free.** A reader here cannot be preempted inside
 * one: nk switches tasks only from the timer handler or an explicit yield,
 * and no RCU reader in this shim does either. So by the time any writer runs,
 * every reader has finished, and synchronize_rcu has nothing to wait for.
 * The moment nk preempts inside an rcu_read_lock, this becomes wrong and
 * silently so -- it is the assumption most likely to be invalidated by
 * future work on the scheduler.
 */

#include <linux/bottom_half.h>
#include <linux/interrupt.h>
#include <linux/rcupdate.h>
#include <linux/spinlock.h>

#include "nk.h"

void __rcu_read_lock(void) { }
void __rcu_read_unlock(void) { }
void synchronize_net(void) { }

/*
 * Only the enable half is a real function -- the disable half is a static
 * inline that adds to preempt_count, so defining it here is a redefinition
 * rather than an implementation.
 *
 * And it is a no-op, which is a claim worth being explicit about. In Linux,
 * disabling bottom halves stops softirqs running on the return path from an
 * interrupt. nk has no such path: NAPI work runs on an ordinary kernel
 * thread, scheduled like any other. So the thing local_bh_disable excludes
 * does not exist here to be excluded, and preempt_count -- which the inline
 * has already adjusted -- is what any code that cares actually reads.
 *
 * This stops being true if nk ever runs softirq work from the interrupt
 * return path, which is the obvious way to make networking faster.
 */
void __local_bh_enable_ip(unsigned long ip, unsigned int cnt)
{
	(void)ip;
	(void)cnt;
}

static unsigned long long bh_flags;
static int bh_depth;

/*
 * The _bh spinlocks do mask interrupts, unlike local_bh_disable above: they
 * are taken around data an interrupt handler also touches, and there the
 * exclusion is real.
 */
void _raw_spin_lock_bh(raw_spinlock_t *lock)
{
	unsigned long long flags;

	(void)lock;
	flags = nk_irq_save();
	if (bh_depth++ == 0)
		bh_flags = flags;
}

void _raw_spin_unlock_bh(raw_spinlock_t *lock)
{
	(void)lock;
	if (--bh_depth == 0)
		nk_irq_restore(bh_flags);
}

/*
 * Always succeeds. A trylock that can fail needs somebody else to be holding
 * the lock, and with one CPU and no preemption inside a critical section
 * there is nobody. Returning failure instead would send callers down their
 * contended path, which is the one nk has never executed.
 */
int _raw_spin_trylock(raw_spinlock_t *lock)
{
	(void)lock;
	return 1;
}

/*
 * Called when preempt_count reaches zero with a reschedule pending. nk's
 * scheduler is driven by the timer rather than by preempt_count, so the
 * request has already been served or will be at the next tick.
 */
void preempt_schedule(void)
{
}

void refcount_warn_saturate(refcount_t *r, enum refcount_saturation_type t)
{
	(void)r;
	pr_warn("refcount saturated (%d) -- a leak or a double free\n", (int)t);
}
