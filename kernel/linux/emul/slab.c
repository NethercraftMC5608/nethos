// SPDX-License-Identifier: GPL-2.0
/*
 * kmalloc and kfree, on nk's heap.
 *
 * The awkward part is that `kmalloc` is not a function. In modern Linux it is
 * an inline that picks a slab cache out of `kmalloc_caches[type][index]` and
 * calls `__kmalloc_cache_noprof(cache, flags, size)`, falling back to
 * `__kmalloc_noprof(size, flags)` when the size is not a compile-time
 * constant. So there is nothing named kmalloc to implement -- what has to
 * exist is those two functions and that array.
 *
 * The array is left empty and the cache argument ignored. nk has one
 * allocator, not a per-size-class set of them, so the cache the inline
 * carefully selected carries no information we can use. The size does, and it
 * is passed alongside. That is the entire trick.
 *
 * Everything is aligned to NK_KMALLOC_ALIGN rather than to the request.
 * Drivers rely on kmalloc'd memory being safe to hand to a DMA engine, which
 * on Linux means at least ARCH_KMALLOC_MINALIGN, and a driver that gets less
 * fails at the device rather than at the allocation.
 */

#include <linux/slab.h>
#include <linux/gfp.h>
#include <linux/string.h>

#include "nk.h"

/*
 * Referenced by the kmalloc inline before it calls us. Every entry stays NULL:
 * we never dereference the cache, and neither does anything else here.
 */
struct kmem_cache *kmalloc_caches[NR_KMALLOC_TYPES][KMALLOC_SHIFT_HIGH + 1];

static void *alloc(size_t size, gfp_t flags)
{
	void *p = nk_alloc(size, NK_KMALLOC_ALIGN);

	if (p && (flags & __GFP_ZERO))
		memset(p, 0, size);
	return p;
}

void *__kmalloc_noprof(size_t size, gfp_t flags)
{
	return alloc(size, flags);
}

void *__kmalloc_cache_noprof(struct kmem_cache *s, gfp_t flags, size_t size)
{
	(void)s;
	return alloc(size, flags);
}

void kfree(const void *p)
{
	nk_free((void *)p);
}

/*
 * alloc_pages_exact is whole pages and an exact length. virtio_ring uses it
 * for the rings themselves, which must be physically contiguous -- hence
 * nk_alloc_pages rather than the heap, which makes no such promise.
 */
void *alloc_pages_exact_noprof(size_t size, gfp_t flags)
{
	unsigned long pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
	void *p = nk_alloc_pages(pages);

	if (p && (flags & __GFP_ZERO))
		memset(p, 0, pages * PAGE_SIZE);
	return p;
}

void free_pages_exact(void *virt, size_t size)
{
	/*
	 * Leaked, on purpose and visibly. nk's frame allocator frees one page
	 * at a time and this is a run of them; handing back only the first
	 * would be worse than not handing back any, because it would look
	 * correct. A virtqueue is allocated once at probe and freed at
	 * teardown, which nk does not do, so nothing leaks in practice yet.
	 */
	(void)virt;
	(void)size;
}
