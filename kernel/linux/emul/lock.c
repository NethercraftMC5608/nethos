// SPDX-License-Identifier: GPL-2.0
/*
 * Spinlocks and mutexes, for a kernel with one CPU.
 *
 * There is no lock here, and that is correct rather than lazy. nk parks every
 * CPU but affinity 0 in boot.s, so no two threads ever execute at once and a
 * spinlock has nothing to spin against. What a spinlock still has to do is
 * the *other* half of its job: keep an interrupt handler out of a section a
 * thread is halfway through. That is the interrupt mask, and it is real.
 *
 * The plain `spin_lock` variants therefore mask interrupts too. Linux's do
 * not -- they rely on the caller knowing the lock is never taken from
 * interrupt context -- but nk has no lockdep to check that claim, and masking
 * unnecessarily costs a few cycles where getting it wrong costs a deadlock
 * nobody can see.
 *
 * The moment a second CPU runs, every one of these becomes wrong. That is the
 * single largest thing standing between nk and SMP.
 */

#include <linux/mutex.h>
#include <linux/spinlock.h>

#include "nk.h"

/*
 * The plain lock/unlock pair has nowhere to put the interrupt state -- Linux's
 * signature returns nothing -- so it goes in a counter here. One CPU means one
 * counter is enough, and the depth is what makes nesting work: only the
 * outermost unlock restores, so an inner critical section cannot re-enable
 * interrupts underneath an outer one.
 *
 * This composes with the irqsave pair below, which carries its own flags: a
 * plain lock taken inside an irqsave section saves the already-masked state
 * and restores exactly that.
 */
static int depth;
static unsigned long long outermost;

static void mask(void)
{
	unsigned long long flags = nk_irq_save();

	if (depth++ == 0)
		outermost = flags;
}

static void unmask(void)
{
	if (--depth == 0)
		nk_irq_restore(outermost);
}

void _raw_spin_lock(raw_spinlock_t *lock)
{
	(void)lock;
	mask();
}

void _raw_spin_unlock(raw_spinlock_t *lock)
{
	(void)lock;
	unmask();
}

void _raw_spin_lock_irq(raw_spinlock_t *lock)
{
	(void)lock;
	mask();
}

void _raw_spin_unlock_irq(raw_spinlock_t *lock)
{
	(void)lock;
	unmask();
}

unsigned long _raw_spin_lock_irqsave(raw_spinlock_t *lock)
{
	(void)lock;
	return nk_irq_save();
}

void _raw_spin_unlock_irqrestore(raw_spinlock_t *lock, unsigned long flags)
{
	(void)lock;
	nk_irq_restore(flags);
}

/*
 * Mutexes may sleep, so these yield rather than mask. With one CPU and a
 * preemptive scheduler that is enough to be correct: the holder will run
 * again, and the waiter gives up its slice until it does.
 */
void mutex_init_generic(struct mutex *lock)
{
	atomic_long_set(&lock->owner, 0);
}

void mutex_lock(struct mutex *lock)
{
	while (atomic_long_cmpxchg(&lock->owner, 0, 1) != 0)
		nk_yield();
}

void mutex_unlock(struct mutex *lock)
{
	atomic_long_set(&lock->owner, 0);
}
