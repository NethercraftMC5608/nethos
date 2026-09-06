// SPDX-License-Identifier: GPL-2.0
/*
 * The rest of the allocation surface: managed allocations, pages, and the
 * kv* pair.
 *
 * `devm_` is Linux's device-managed allocation: memory attached to a device
 * and freed automatically when the driver unbinds. nk never unbinds a driver
 * -- there is no hotplug, no module unload and no unbind -- so the list that
 * would make the freeing automatic would never be walked. These are therefore
 * plain allocations, and they leak exactly once per device at probe, which is
 * a bounded amount that nothing ever reclaims.
 */

#include <linux/device.h>
#include <linux/gfp.h>
#include <linux/mm.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

void *devm_kmalloc(struct device *dev, size_t size, gfp_t gfp)
{
	void *p;

	(void)dev;
	p = nk_alloc(size, NK_KMALLOC_ALIGN);
	if (p && (gfp & __GFP_ZERO))
		memset(p, 0, size);
	return p;
}

void devm_kfree(struct device *dev, const void *p)
{
	(void)dev;
	nk_free((void *)p);
}

void *kmemdup_noprof(const void *src, size_t len, gfp_t gfp)
{
	void *p = nk_alloc(len, NK_KMALLOC_ALIGN);

	(void)gfp;
	if (p)
		memcpy(p, src, len);
	return p;
}

/*
 * kvmalloc tries kmalloc and falls back to vmalloc for large requests, so
 * that a big allocation does not need contiguous physical memory. nk has no
 * vmalloc, so every allocation is physically contiguous and this is just
 * kmalloc -- which means a large one can fail where Linux's would not.
 */
/*
 * Declared through Linux's own parameter macro rather than spelled out. The
 * real signature carries allocation-profiling bucket and token arguments that
 * differ with the config, and writing them by hand is a signature that is
 * right for one kernel build and silently wrong for the next.
 */
void *__kvmalloc_node_noprof(DECL_KMALLOC_PARAMS(size, b, token),
			     unsigned long align, gfp_t flags, int node)
{
	void *p;

	(void)node;
	p = nk_alloc(size, align > NK_KMALLOC_ALIGN ? align : NK_KMALLOC_ALIGN);
	if (p && (flags & __GFP_ZERO))
		memset(p, 0, size);
	return p;
}

void kvfree(const void *p)
{
	nk_free((void *)p);
}

/* --- pages ------------------------------------------------------------- */

struct page *alloc_pages_noprof(gfp_t gfp, unsigned int order)
{
	void *p = nk_alloc_pages(1UL << order);

	if (!p)
		return NULL;
	if (gfp & __GFP_ZERO)
		memset(p, 0, PAGE_SIZE << order);
	return virt_to_page(p);
}

unsigned long get_free_pages_noprof(gfp_t gfp, unsigned int order)
{
	void *p = nk_alloc_pages(1UL << order);

	if (p && (gfp & __GFP_ZERO))
		memset(p, 0, PAGE_SIZE << order);
	return (unsigned long)p;
}

/*
 * Not freed, and visibly so. nk's frame allocator hands back one page at a
 * time and these are runs of them; returning only the first would look
 * correct and would corrupt the free list on the next allocation. Fixing it
 * properly means a buddy allocator, which is the right answer and is not
 * this file's job.
 */
void __free_pages(struct page *page, unsigned int order)
{
	(void)page;
	(void)order;
}

void free_pages(unsigned long addr, unsigned int order)
{
	(void)addr;
	(void)order;
}

void __folio_put(struct folio *folio)
{
	(void)folio;
}

/*
 * A no-op because nk's data caches are coherent with everything that reads
 * them: one CPU, no non-coherent DMA master, and paging.rs maps all of RAM
 * Normal Inner-Shareable write-back. On a machine with a non-coherent device
 * this is the first thing that would have to become real, and the symptom of
 * leaving it would be data that is correct in memory and stale at the device.
 */
void flush_dcache_page(struct page *page)
{
	(void)page;
}
