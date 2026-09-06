// SPDX-License-Identifier: GPL-2.0
/*
 * Time, in the three shapes Linux asks for it.
 *
 * `jiffies` is the one that matters and the one that is easy to get wrong.
 * It is not a function: drivers read the variable directly, and a great deal
 * of driver code loops until it changes. Left at zero it is not merely
 * inaccurate -- every timeout in every driver becomes infinite, and the
 * failure looks like a device that never answers. nk's timer tick writes it.
 */

#include <linux/jiffies.h>
#include <linux/ktime.h>
#include <linux/sched/clock.h>

#include "nk.h"

/*
 * Linux starts jiffies near wrap-around on purpose, so that code which
 * mishandles the wrap fails immediately rather than after fifty days. Keeping
 * that is free and nk has no reason to be gentler than Linux is.
 */
unsigned long volatile jiffies = INITIAL_JIFFIES;

/* Called from nk's timer interrupt, once per tick. */
void nk_tick(void);
void nk_tick(void)
{
	jiffies++;
}

ktime_t ktime_get(void)
{
	/* From the tick rather than the counter: it is what jiffies agrees
	 * with, and a clock that disagrees with the timeouts measured against
	 * it is worse than a coarse one. */
	return (u64)nk_ticks() * (NSEC_PER_SEC / (u64)nk_hz());
}

u64 sched_clock(void)
{
	return ktime_get();
}
