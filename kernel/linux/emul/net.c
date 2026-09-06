// SPDX-License-Identifier: GPL-2.0
/*
 * Enough of the network stack for a driver to be given a packet and to hand
 * one back. There is no network stack here at all.
 *
 * The same shape as block.c, one layer up: what virtio_net.c sees of Linux is
 * small and can be stated in a sentence. It allocates a net_device, registers
 * it, is opened, adds NAPI contexts with a poll function, and thereafter:
 *
 *   - nk calls ndo_start_xmit to send;
 *   - the device interrupts, the driver schedules NAPI, nk's thread calls
 *     poll, and the driver hands packets up through gro_receive_skb.
 *
 * That last call is where a received frame arrives, and it is the only place
 * nk has to be. Everything between it and the wire -- the virtqueues, the
 * headers, the descriptor chains, the interrupt handling -- is the real
 * driver.
 *
 * What is absent, deliberately: no protocol stack, no routing, no sockets, no
 * qdisc, no rtnetlink. nk builds Ethernet frames by hand and looks at the
 * ones that come back. That is the right amount for proving a driver works
 * and is nowhere near a network.
 */

#include <linux/etherdevice.h>
#include <linux/netdevice.h>
#include <net/netdev_rx_queue.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

#define MAX_NAPI 8

static struct net_device *the_dev;
static struct napi_struct *napis[MAX_NAPI];
static int nnapi;

/* Set by __napi_schedule from interrupt context, cleared by the poll thread. */
static volatile int napi_pending[MAX_NAPI];

/* Where a received frame is left for nk. One slot: nk consumes each before
 * asking for the next, and a queue would be a queue nothing drains faster. */
static u8 rx_buf[2048];
static volatile unsigned int rx_len;

/*
 * The general allocator. `alloc_etherdev_mqs` is Linux's own -- net/ethernet/
 * eth.c is in the port -- and it calls this with `ether_setup`, which is also
 * Linux's and fills in the MTU, address length, broadcast address and flags
 * that make the device an Ethernet one. Implementing the specific one instead
 * meant writing all of that by hand, and getting `ether_setup` slightly wrong
 * gives a device that works until something inspects it.
 */
struct net_device *alloc_netdev_mqs(int sizeof_priv, const char *name,
				    unsigned char name_assign_type,
				    void (*setup)(struct net_device *),
				    unsigned int txqs, unsigned int rxqs)
{
	struct net_device *dev;
	unsigned int total;

	(void)name_assign_type;
	/* netdev_priv() is a fixed offset past the net_device, so the two are
	 * one allocation and the padding between them has to match Linux's or
	 * every private field is read at the wrong address. */
	total = ALIGN(sizeof(*dev), NETDEV_ALIGN) + sizeof_priv;
	dev = nk_alloc(total, NETDEV_ALIGN);
	if (!dev)
		return NULL;
	memset(dev, 0, total);

	dev->num_tx_queues = txqs;
	dev->real_num_tx_queues = txqs;
	dev->num_rx_queues = rxqs;
	dev->real_num_rx_queues = rxqs;

	/*
	 * The queue arrays, which Linux's own alloc_netdev_mqs allocates and
	 * this did not. `netdev_get_tx_queue(dev, i)` is `&dev->_tx[i]`, so a
	 * null `_tx` is not a null pointer the driver checks -- it is a small
	 * address the driver writes a byte-queue-limit counter into.
	 *
	 * It did not fault. nk mapped the whole first gigabyte as one Device
	 * block for peripherals that are not there, so the bottom of the
	 * address space was writable memory that went nowhere. Narrowing that
	 * mapping to the 34MB the machine actually has is what turned this
	 * into a fault at `start_xmit+0x528` instead of a packet counter
	 * quietly written to address 0x88.
	 */
	dev->_tx = nk_alloc(txqs * sizeof(*dev->_tx), 64);
	dev->_rx = nk_alloc(rxqs * sizeof(*dev->_rx), 64);
	if (!dev->_tx || !dev->_rx) {
		nk_free(dev);
		return NULL;
	}
	memset(dev->_tx, 0, txqs * sizeof(*dev->_tx));
	memset(dev->_rx, 0, rxqs * sizeof(*dev->_rx));
	for (unsigned int i = 0; i < txqs; i++)
		dev->_tx[i].dev = dev;
	for (unsigned int i = 0; i < rxqs; i++)
		dev->_rx[i].dev = dev;
	dev->tx_queue_len = 1000;
	INIT_LIST_HEAD(&dev->napi_list);

	/* dev_addr is a pointer, not an array, and Linux aims it at a list
	 * entry it allocates here. Left null, every write through
	 * dev_addr_mod goes to address zero and the interface comes up with
	 * no hardware address -- which reads as a device that did not report
	 * one, rather than as a bug in this file. */
	dev->dev_addr = nk_alloc(MAX_ADDR_LEN, 8);
	if (!dev->dev_addr) {
		nk_free(dev);
		return NULL;
	}
	memset((void *)dev->dev_addr, 0, MAX_ADDR_LEN);

	if (setup)
		setup(dev);

	/* Linux allocates "eth%d" and fills in the number when the device is
	 * registered. nk has one, so it is eth0. */
	if (name)
		snprintf(dev->name, sizeof(dev->name), name, 0);
	return dev;
}

void free_netdev(struct net_device *dev)
{
	nk_free(dev);
}

int register_netdevice(struct net_device *dev)
{
	the_dev = dev;
	if (dev->netdev_ops && dev->netdev_ops->ndo_init) {
		int ret = dev->netdev_ops->ndo_init(dev);

		if (ret)
			return ret;
	}
	return 0;
}

void unregister_netdev(struct net_device *dev)
{
	(void)dev;
	the_dev = NULL;
}

/* rtnl is the lock protecting the whole netdev configuration tree from
 * concurrent reconfiguration. nk reconfigures nothing and has one CPU. */
void rtnl_lock(void) { }
void rtnl_unlock(void) { }

void netif_tx_lock(struct net_device *dev) { (void)dev; }
void netif_tx_unlock(struct net_device *dev) { (void)dev; }

/*
 * Carrier and queue state. Every one of these is a signal to a layer above
 * the driver -- "stop giving me packets", "the link came up" -- and nk is
 * that layer, submitting one packet at a time and waiting. There is nothing
 * to tell.
 */
void netif_carrier_on(struct net_device *dev) { (void)dev; }
void netif_carrier_off(struct net_device *dev) { (void)dev; }
void netif_device_attach(struct net_device *dev) { (void)dev; }
void netif_device_detach(struct net_device *dev) { (void)dev; }
void netif_tx_stop_all_queues(struct net_device *dev) { (void)dev; }
void netif_tx_wake_queue(struct netdev_queue *q) { (void)q; }
void netif_schedule_queue(struct netdev_queue *q) { (void)q; }
void netif_queue_set_napi(struct net_device *dev, unsigned int qi,
			  enum netdev_queue_type type, struct napi_struct *n)
{ (void)dev; (void)qi; (void)type; (void)n; }

int netif_set_real_num_tx_queues(struct net_device *dev, unsigned int txq)
{
	dev->real_num_tx_queues = txq;
	return 0;
}

int netif_set_real_num_rx_queues(struct net_device *dev, unsigned int rxq)
{
	dev->real_num_rx_queues = rxq;
	return 0;
}

/* --- NAPI ------------------------------------------------------------- */

void netif_napi_add_weight_locked(struct net_device *dev,
				  struct napi_struct *napi,
				  int (*poll)(struct napi_struct *, int),
				  int weight)
{
	if (nnapi == MAX_NAPI)
		return;
	napi->dev = dev;
	napi->poll = poll;
	napi->weight = weight;
	napi->napi_id = nnapi;
	napis[nnapi++] = napi;
}

void __netif_napi_del_locked(struct napi_struct *napi)
{
	(void)napi;
}

void napi_enable(struct napi_struct *napi) { (void)napi; }
void napi_disable(struct napi_struct *napi) { (void)napi; }

bool napi_schedule_prep(struct napi_struct *napi)
{
	(void)napi;
	/* "Is it worth scheduling?" -- always, here: nk's poll thread is the
	 * only consumer and it is not already running when this is called
	 * from an interrupt. */
	return true;
}

void __napi_schedule(struct napi_struct *napi)
{
	unsigned int i = napi->napi_id;

	if (i < MAX_NAPI)
		napi_pending[i] = 1;
}

bool napi_complete_done(struct napi_struct *napi, int work_done)
{
	unsigned int i = napi->napi_id;

	(void)work_done;
	if (i < MAX_NAPI)
		napi_pending[i] = 0;
	/* True means "polling really is finished"; the driver re-enables the
	 * device's interrupt on the strength of it. */
	return true;
}

/*
 * A received frame, handed up by the driver. The end of the receive path and
 * the only line in this file that carries data rather than state.
 */
gro_result_t gro_receive_skb(struct gro_node *gro, struct sk_buff *skb)
{
	const u8 *start = skb->data;
	unsigned int len = skb->len;

	(void)gro;
	/*
	 * From the MAC header, not from skb->data. eth_type_trans has already
	 * pulled the fourteen-byte Ethernet header off -- that is its job, so
	 * that the layer above sees only its own protocol -- and nk *is* the
	 * layer above but wants the whole frame. skb_mac_header points back at
	 * it, and the header is still in the buffer, only behind `data`.
	 */
	if (skb_mac_header_was_set(skb)) {
		start = skb_mac_header(skb);
		len += skb->data - start;
	}
	if (!rx_len && len && len <= sizeof(rx_buf)) {
		memcpy(rx_buf, start, len);
		/* Length last: nk polls it, and a non-zero length has to mean
		 * the bytes are already there. */
		rx_len = len;
	}
	kfree_skb(skb);
	return GRO_NORMAL;
}

/* --- what nk calls ----------------------------------------------------- */

int nk_net_up(unsigned char *mac);
int nk_net_up(unsigned char *mac)
{
	int ret;

	if (!the_dev || !the_dev->netdev_ops || !the_dev->netdev_ops->ndo_open)
		return -ENODEV;
	/* register_netdevice does not open a device -- in Linux that is
	 * `ip link set up`, from userspace. nk is the userspace. */
	ret = the_dev->netdev_ops->ndo_open(the_dev);
	if (ret)
		return ret;
	memcpy(mac, the_dev->dev_addr, ETH_ALEN);
	return 0;
}

int nk_net_xmit(const void *frame, unsigned int len);
int nk_net_xmit(const void *frame, unsigned int len)
{
	struct sk_buff *skb;

	if (!the_dev)
		return -ENODEV;
	skb = __alloc_skb(len + NET_SKB_PAD + LL_RESERVED_SPACE(the_dev),
			  GFP_KERNEL, 0, -1);
	if (!skb)
		return -ENOMEM;
	skb_reserve(skb, NET_SKB_PAD);
	memcpy(skb_put(skb, len), frame, len);
	skb->dev = the_dev;
	skb->protocol = eth_type_trans(skb, the_dev);
	/* eth_type_trans pulls the header off, and the driver wants it on. */
	skb_push(skb, ETH_HLEN);

	return the_dev->netdev_ops->ndo_start_xmit(skb, the_dev) == NETDEV_TX_OK
		       ? 0
		       : -EBUSY;
}

/* Run any NAPI context the driver has scheduled. Called from nk's thread. */
void nk_net_poll(void);
void nk_net_poll(void)
{
	int i;

	for (i = 0; i < nnapi; i++) {
		if (!napi_pending[i])
			continue;
		napi_pending[i] = 0;
		napis[i]->poll(napis[i], napis[i]->weight);
	}
}

/* Take the last received frame, if there is one. */
unsigned int nk_net_recv(void *out, unsigned int max);
unsigned int nk_net_recv(void *out, unsigned int max)
{
	unsigned int n = rx_len;

	if (!n)
		return 0;
	if (n > max)
		n = max;
	memcpy(out, rx_buf, n);
	rx_len = 0;
	return n;
}
