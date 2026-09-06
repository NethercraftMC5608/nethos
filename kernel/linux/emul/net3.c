// SPDX-License-Identifier: GPL-2.0
/*
 * Three subsystems a modern network driver registers with at probe time and
 * which nk does not have: GRO offloads, XDP, and page pools.
 *
 * None is optional to *call* -- virtio_net registers with all three before it
 * will carry a packet -- and none does anything nk needs. So each is answered
 * in the way that lets the driver take its ordinary path: registration
 * succeeds, and the machinery behind it is absent.
 *
 * Page pools are the one with a real cost. Linux's exists so that receive
 * buffers can be recycled without going back to the page allocator and,
 * with a device that can DMA to them, without remapping. nk's frames go
 * straight back to the frame allocator, which is slower and correct. A
 * driver that depended on recycling for correctness rather than speed would
 * break here; virtio_net does not.
 */

#include <linux/netdevice.h>
#include <net/page_pool/helpers.h>
#include <net/page_pool/types.h>
#include <net/xdp.h>

#include "nk.h"

/* --- GRO offloads ------------------------------------------------------ */

/*
 * An offload is a per-protocol hook for coalescing received segments before
 * they go up the stack. nk has no stack to go up, and gro_receive_skb hands
 * every frame straight to the kernel, so there is nothing to coalesce.
 */
void dev_add_offload(struct packet_offload *po) { (void)po; }
void dev_remove_offload(struct packet_offload *po) { (void)po; }

/* --- XDP --------------------------------------------------------------- */

int __xdp_rxq_info_reg(struct xdp_rxq_info *xdp_rxq, struct net_device *dev,
		       u32 queue_index, unsigned int napi_id, u32 frag_size)
{
	(void)napi_id;
	memset(xdp_rxq, 0, sizeof(*xdp_rxq));
	xdp_rxq->dev = dev;
	xdp_rxq->queue_index = queue_index;
	xdp_rxq->frag_size = frag_size;
	return 0;
}

void xdp_rxq_info_unreg(struct xdp_rxq_info *xdp_rxq) { (void)xdp_rxq; }

int xdp_rxq_info_reg_mem_model(struct xdp_rxq_info *xdp_rxq,
			       enum xdp_mem_type type, void *allocator)
{
	(void)type;
	(void)allocator;
	xdp_rxq->mem.type = type;
	return 0;
}

/*
 * There is no BPF in nk, so no program can ever be attached and the redirect
 * and return paths below are unreachable rather than unimplemented. They halt
 * rather than returning quietly, because reaching one means a program *was*
 * attached, and continuing would run a packet through machinery that is not
 * there.
 */
void xdp_warn(const char *msg, const char *func, const int line)
{
	pr_warn("xdp: %s (%s:%d)\n", msg, func, line);
}

void xdp_return_frame(struct xdp_frame *xdpf) { (void)xdpf; nk_halt(); }
void xdp_return_frame_rx_napi(struct xdp_frame *xdpf) { (void)xdpf; nk_halt(); }
void xdp_do_flush(void) { }

/* --- page pools -------------------------------------------------------- */

/*
 * A pool is one allocation and no recycling: every "get" is a fresh page and
 * every "put" hands it straight back. Linux's version keeps a per-CPU cache
 * and a ring, and maps each page for DMA once rather than per use -- both of
 * which are performance properties, and nk's identity map makes the second
 * meaningless anyway.
 */
struct page_pool *page_pool_create(const struct page_pool_params *params)
{
	struct page_pool *pool = nk_alloc(sizeof(*pool), 8);

	if (!pool)
		return ERR_PTR(-ENOMEM);
	memset(pool, 0, sizeof(*pool));
	if (params)
		pool->p = params->fast;
	return pool;
}

void page_pool_destroy(struct page_pool *pool)
{
	nk_free(pool);
}

struct page *page_pool_alloc_pages(struct page_pool *pool, gfp_t gfp)
{
	void *p;

	(void)pool;
	(void)gfp;
	p = nk_alloc_pages(1);
	return p ? virt_to_page(p) : NULL;
}

netmem_ref page_pool_alloc_netmems(struct page_pool *pool, gfp_t gfp)
{
	struct page *page = page_pool_alloc_pages(pool, gfp);

	return page ? page_to_netmem(page) : 0;
}

netmem_ref page_pool_alloc_frag_netmem(struct page_pool *pool,
				       unsigned int *offset, unsigned int size,
				       gfp_t gfp)
{
	/* No sub-page fragments: a whole page each time, offset zero. The
	 * waste is real and is the price of not keeping a fragment cursor
	 * that would have to be right under concurrent refill. */
	(void)size;
	*offset = 0;
	return page_pool_alloc_netmems(pool, gfp);
}

void page_pool_put_unrefed_netmem(struct page_pool *pool, netmem_ref netmem,
				  unsigned int dma_sync_size, bool allow_direct)
{
	(void)pool;
	(void)dma_sync_size;
	(void)allow_direct;
	(void)netmem;
	/* Deliberately dropped rather than freed. nk's frame allocator frees
	 * single pages and a netmem may be a fragment of one; handing back a
	 * fragment would corrupt the free list. Receive buffers therefore
	 * leak, boundedly, and this is the first thing to fix if nk ever runs
	 * a real traffic load. */
}
