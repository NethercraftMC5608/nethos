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
#include <linux/string.h>
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
 * A real allocation, because CONFIG_CPUMASK_OFFSTACK is set and
 * `cpumask_var_t` is therefore a *pointer*: the caller's variable is
 * uninitialised until this fills it in.
 *
 * This returned true without writing anything, on the reasoning that a mask
 * of one CPU is small enough to live in the caller's variable. That is what
 * happens when OFFSTACK is *off*, and it is not. So `virtnet_set_affinity`
 * took the null it was handed and did an atomic read-modify-write through it.
 *
 * It did not fault. nk mapped the whole first gigabyte as one Device block
 * for peripherals that are not there, so address zero was writable memory
 * that went nowhere -- and the bug sat quiet until the device mapping was
 * narrowed to the 34MB the machine actually has. Mapping what is not there
 * costs more than the page tables it saves.
 */
bool alloc_cpumask_var_node(cpumask_var_t *mask, gfp_t flags, int node)
{
	(void)flags;
	(void)node;
	*mask = nk_alloc(sizeof(struct cpumask), 8);
	if (!*mask)
		return false;
	memset(*mask, 0, sizeof(struct cpumask));
	return true;
}

void free_cpumask_var(cpumask_var_t mask)
{
	nk_free(mask);
}

/* The workqueue drivers reach for when they have no reason to want their
 * own. nk has exactly one worker thread, so this is that one. */
struct workqueue_struct *system_percpu_wq;
