// SPDX-License-Identifier: GPL-2.0
/*
 * One CPU, and the machinery Linux has for having more than one.
 *
 * Every mask here has exactly bit 0 set and nothing ever changes it. That is
 * not a placeholder: nk parks every core but affinity 0 in boot.s, so it is
 * the truth, and a driver that spreads its queues across the CPUs this
 * reports will correctly put all of them on one.
 *
 * CPU hotplug is the interesting part. A driver registers a pair of callbacks
 * to be run when a CPU comes up or goes down, and virtio-net uses them to
 * rebalance its queue affinity. nk's CPUs never come or go, so the startup
 * callback is invoked once, immediately, for CPU 0 -- which is exactly what
 * Linux does for every already-online CPU at registration time when `invoke`
 * is set. The teardown is never called because nothing ever goes down.
 */

#include <linux/cpu.h>
#include <linux/cpuhotplug.h>
#include <linux/cpumask.h>
#include <linux/slab.h>
#include <linux/workqueue.h>

#include "nk.h"

/* Bit 0, and only bit 0. A struct rather than a pointer, because that is how
 * cpumask.h declares it and the inlines index into it directly. */
struct cpumask __cpu_online_mask = { .bits = { 1 } };
struct cpumask __cpu_possible_mask = { .bits = { 1 } };
struct cpumask __cpu_present_mask = { .bits = { 1 } };
unsigned int nr_cpu_ids = 1;
atomic_t __num_online_cpus = ATOMIC_INIT(1);

void cpus_read_lock(void) { }
void cpus_read_unlock(void) { }

/*
 * The state numbers a driver asks for are either a fixed enum value or
 * CPUHP_AP_ONLINE_DYN, meaning "allocate me one". Linux hands out descending
 * numbers from a pool; nk hands out ascending ones from a counter, because
 * nothing here ever looks at the ordering they encode.
 */
static int dyn_state = CPUHP_AP_ONLINE_DYN;

int __cpuhp_setup_state(enum cpuhp_state state, const char *name, bool invoke,
			int (*startup)(unsigned int cpu),
			int (*teardown)(unsigned int cpu),
			bool multi_instance)
{
	(void)name;
	(void)teardown;
	(void)multi_instance;
	if (invoke && startup) {
		int ret = startup(0);

		if (ret)
			return ret;
	}
	return state == CPUHP_AP_ONLINE_DYN ? dyn_state++ : 0;
}

void __cpuhp_remove_state(enum cpuhp_state state, bool invoke)
{
	(void)state;
	(void)invoke;
}

int __cpuhp_state_add_instance(enum cpuhp_state state, struct hlist_node *node,
			       bool invoke)
{
	(void)state;
	(void)node;
	(void)invoke;
	return 0;
}

int __cpuhp_state_remove_instance(enum cpuhp_state state,
				  struct hlist_node *node, bool inv)
{
	(void)state;
	(void)node;
	(void)inv;
	return 0;
}

/*
 * A cpumask is one word here, so "allocate" is a lie that costs nothing --
 * CPUMASK_OFFSTACK is not set for a mask this small and the caller's variable
 * already is the mask.
 */
bool alloc_cpumask_var_node(cpumask_var_t *mask, gfp_t flags, int node)
{
	(void)mask;
	(void)flags;
	(void)node;
	return true;
}

void free_cpumask_var(cpumask_var_t mask)
{
	(void)mask;
}

/* The workqueue drivers reach for when they have no reason to want their
 * own. nk has exactly one worker thread, so this is that one. */
struct workqueue_struct *system_percpu_wq;
