// SPDX-License-Identifier: GPL-2.0
/*
 * sk_buff: allocation, and the pointer arithmetic every driver depends on.
 *
 * Not taken from net/core/skbuff.c, and the decision is worth recording
 * because it goes the other way from lib/vsprintf.c and lib/scatterlist.c.
 * skbuff.c is seven thousand lines and reaches into page pools, sockets,
 * memory accounting, GRO, zero-copy, fragment lists and the netlink dumping
 * of all of it. Taking it means taking most of net/core with it. What a
 * driver actually needs is much smaller and is entirely about *layout*:
 *
 *     head           data                tail            end
 *      |--headroom--->|-------len-------->|--tailroom---->|
 *                                                          skb_shared_info
 *
 * head/data/tail/end are read directly by driver code and by the inline
 * helpers in skbuff.h, so they have to be exactly right; the shared_info
 * sitting immediately past `end` is where nr_frags lives, and a driver that
 * reads a non-zero one from uninitialised memory will follow a fragment
 * pointer into nothing.
 *
 * **Linear buffers only.** nk allocates one contiguous data area per skb and
 * never sets nr_frags. That means the mergeable-buffer and big-packet receive
 * paths in virtio_net are not exercised -- run QEMU with `mrg_rxbuf=off` and
 * the driver takes the small-packet path, which is the one this supports.
 */

#include <linux/netdevice.h>
#include <linux/skbuff.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

static struct sk_buff *alloc(unsigned int size)
{
	struct sk_buff *skb;
	u8 *data;

	skb = nk_alloc(sizeof(*skb), NK_KMALLOC_ALIGN);
	if (!skb)
		return NULL;
	memset(skb, 0, sizeof(*skb));

	size = SKB_DATA_ALIGN(size);
	data = nk_alloc(size + SKB_DATA_ALIGN(sizeof(struct skb_shared_info)),
			NK_KMALLOC_ALIGN);
	if (!data) {
		nk_free(skb);
		return NULL;
	}

	skb->head = data;
	skb->data = data;
	skb_reset_tail_pointer(skb);
	skb->end = skb->tail + size;
	skb->len = 0;
	skb->truesize = size + sizeof(*skb);
	refcount_set(&skb->users, 1);

	/* Zeroed, not left as it came: nr_frags is read before it is written
	 * on every receive path, and a stale one sends the driver after a
	 * fragment that does not exist. */
	memset(skb_shinfo(skb), 0, sizeof(struct skb_shared_info));
	return skb;
}

struct sk_buff *__alloc_skb(unsigned int size, gfp_t gfp, int flags, int node)
{
	(void)gfp;
	(void)flags;
	(void)node;
	return alloc(size);
}

struct sk_buff *napi_alloc_skb(struct napi_struct *napi, unsigned int len)
{
	struct sk_buff *skb = alloc(len + NET_SKB_PAD);

	(void)napi;
	if (skb) {
		skb_reserve(skb, NET_SKB_PAD);
		if (napi && napi->dev)
			skb->dev = napi->dev;
	}
	return skb;
}

/*
 * build_skb wraps a buffer the caller already has, rather than allocating
 * one. virtio_net uses it on the receive path so the data does not have to be
 * copied out of the buffer the device wrote into.
 */
struct sk_buff *build_skb(void *data, unsigned int frag_size)
{
	struct sk_buff *skb = nk_alloc(sizeof(*skb), NK_KMALLOC_ALIGN);
	unsigned int size;

	if (!skb)
		return NULL;
	memset(skb, 0, sizeof(*skb));

	size = frag_size ? frag_size - SKB_DATA_ALIGN(sizeof(struct skb_shared_info))
			 : 0;
	skb->head = data;
	skb->data = data;
	skb_reset_tail_pointer(skb);
	skb->end = skb->tail + size;
	skb->truesize = SKB_TRUESIZE(size);
	refcount_set(&skb->users, 1);
	memset(skb_shinfo(skb), 0, sizeof(struct skb_shared_info));
	return skb;
}

void skb_over_panic(struct sk_buff *skb, unsigned int len, void *here);
void skb_under_panic(struct sk_buff *skb, unsigned int len, void *here);

/*
 * The three that move the data pointers. They are the whole reason the layout
 * at the top of this file has to be exact: every driver and every helper in
 * skbuff.h reads head/data/tail/end directly, and a `len` that disagrees with
 * `tail - data` is a packet whose length nobody can agree on.
 */
void *skb_put(struct sk_buff *skb, unsigned int len)
{
	void *tail = skb_tail_pointer(skb);

	skb->tail += len;
	skb->len += len;
	if (skb->tail > skb->end)
		skb_over_panic(skb, len, __builtin_return_address(0));
	return tail;
}

void *skb_push(struct sk_buff *skb, unsigned int len)
{
	skb->data -= len;
	skb->len += len;
	if (skb->data < skb->head)
		skb_under_panic(skb, len, __builtin_return_address(0));
	return skb->data;
}

void *skb_pull(struct sk_buff *skb, unsigned int len)
{
	if (len > skb->len)
		return NULL;
	skb->len -= len;
	skb->data += len;
	return skb->data;
}

/*
 * skb_put's inline form calls this only to report an overrun.
 * Reaching it means a driver has written past the end of the buffer it was
 * given, which has already happened by the time we are told -- so this
 * stops rather than continues.
 */
void skb_over_panic(struct sk_buff *skb, unsigned int len, void *here)
{
	(void)here;
	pr_err("skb_put overran the buffer: len %u, tailroom %u\n", len,
	       skb_tailroom(skb));
	nk_halt();
}

void skb_under_panic(struct sk_buff *skb, unsigned int len, void *here)
{
	(void)here;
	pr_err("skb_push underran the buffer: len %u, headroom %u\n", len,
	       skb_headroom(skb));
	nk_halt();
}

static void free_skb(struct sk_buff *skb)
{
	if (!skb)
		return;
	if (!refcount_dec_and_test(&skb->users))
		return;
	nk_free(skb->head);
	nk_free(skb);
}

void __kfree_skb(struct sk_buff *skb)
{
	free_skb(skb);
}

/* kfree_skb_reason is an inline over this one. */
void sk_skb_reason_drop(const struct sock *sk, struct sk_buff *skb,
			enum skb_drop_reason reason)
{
	(void)sk;
	(void)reason;
	free_skb(skb);
}

void dev_kfree_skb_any_reason(struct sk_buff *skb, enum skb_drop_reason reason)
{
	(void)reason;
	free_skb(skb);
}

void napi_consume_skb(struct sk_buff *skb, int budget)
{
	(void)budget;
	free_skb(skb);
}

/*
 * The whole point of a linear-only skb: there is never a second fragment to
 * pull into the head, so this is either already satisfied or a request nk
 * cannot meet.
 */
void *__pskb_pull_tail(struct sk_buff *skb, int delta)
{
	return delta <= skb_tailroom(skb) ? skb_tail_pointer(skb) : NULL;
}

int skb_to_sgvec(struct sk_buff *skb, struct scatterlist *sg, int offset,
		 int len)
{
	if (offset + len > (int)skb->len)
		return -EINVAL;
	sg_init_table(sg, 1);
	sg_set_buf(sg, skb->data + offset, len);
	sg_mark_end(sg);
	return 1;
}

/* Checksum offload is refused during feature negotiation, so a partial
 * checksum should never be set up. Reaching this means it was. */
bool skb_partial_csum_set(struct sk_buff *skb, u16 start, u16 off)
{
	(void)skb;
	(void)start;
	(void)off;
	return false;
}
