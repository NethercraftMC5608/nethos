// SPDX-License-Identifier: GPL-2.0
/*
 * The rest of the netdev surface: reporting, addresses, and the several
 * subsystems a modern network driver expects to exist.
 *
 * Almost none of this is on the path a packet takes. It is what a driver
 * calls to describe itself to a system that is watching -- ethtool, sysfs,
 * dynamic interrupt moderation, XDP, page pools, RSS. nk is not watching, so
 * each is answered in the way that leaves the driver's own logic taking its
 * ordinary path rather than an error path.
 */

#include <linux/etherdevice.h>
#include <linux/ethtool.h>
#include <linux/netdevice.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

/*
 * dev_addr_mod exists because Linux keeps a device's hardware address in a
 * list that has to be kept consistent with dev->dev_addr, and drivers must
 * not write the array behind its back. nk has no such list, so the write is
 * the whole operation.
 */
void dev_addr_mod(struct net_device *dev, unsigned int offset,
		  const void *addr, size_t len)
{
	memcpy((void *)(dev->dev_addr + offset), addr, len);
}

/* --- reporting -------------------------------------------------------- */

void netdev_printk(const char *level, const struct net_device *dev,
		   const char *fmt, ...)
{
	va_list args;
	char buf[512];
	int n;

	(void)level;
	nk_console_write("[linux] ", 8);
	if (dev && dev->name[0]) {
		nk_console_write(dev->name, strlen(dev->name));
		nk_console_write(": ", 2);
	}
	va_start(args, fmt);
	n = vscnprintf(buf, sizeof(buf), fmt, args);
	va_end(args);
	if (n > 0)
		nk_console_write(buf, n);
	if (n <= 0 || buf[n - 1] != '\n')
		nk_console_write("\n", 1);
}

#define NETDEV_PRINTER(fn)						\
	void fn(const struct net_device *dev, const char *fmt, ...)	\
	{								\
		va_list args;						\
		char buf[512];						\
		int n;							\
		va_start(args, fmt);					\
		n = vscnprintf(buf, sizeof(buf), fmt, args);		\
		va_end(args);						\
		netdev_printk("", dev, "%s", n > 0 ? buf : "");		\
	}

NETDEV_PRINTER(netdev_err)
NETDEV_PRINTER(netdev_warn)

/* Rate limiting exists to stop a flood of identical messages drowning a log
 * nobody is reading fast enough. nk's log is a serial port and its author is
 * looking at every line. */
int net_ratelimit(void)
{
	return 1;
}

/* --- ethtool ---------------------------------------------------------- */

u32 ethtool_op_get_link(struct net_device *dev)
{
	(void)dev;
	return 1;
}

int ethtool_op_get_ts_info(struct net_device *dev, struct kernel_ethtool_ts_info *ti)
{
	(void)dev;
	(void)ti;
	return 0;
}

void ethtool_sprintf(u8 **data, const char *fmt, ...)
{
	va_list args;
	int n;

	va_start(args, fmt);
	n = vscnprintf((char *)*data, ETH_GSTRING_LEN, fmt, args);
	va_end(args);
	*data += ETH_GSTRING_LEN;
	(void)n;
}

int ethtool_virtdev_set_link_ksettings(struct net_device *dev,
				       const struct ethtool_link_ksettings *cmd,
				       u32 *dev_speed, u8 *dev_duplex)
{
	(void)dev;
	(void)cmd;
	(void)dev_speed;
	(void)dev_duplex;
	return 0;
}

bool netif_is_rxfh_configured(const struct net_device *dev)
{
	(void)dev;
	return false;
}

/* The RSS hash key. Random in Linux so that a remote peer cannot choose which
 * queue its traffic lands on; nk has one queue, so it decides nothing -- but
 * it is filled rather than left as stack rubbish, because the driver hands it
 * to the device. */
void netdev_rss_key_fill(void *buffer, size_t len)
{
	get_random_bytes(buffer, len);
}

int __netif_set_xps_queue(struct net_device *dev, const unsigned long *mask,
			  u16 index, enum xps_map_type type)
{
	/* Transmit packet steering picks a queue per CPU. One of each. */
	(void)dev; (void)mask; (void)index; (void)type;
	return 0;
}

u64 netdev_stat_queue_sum(struct net_device *dev, int rx_start, int rx_end,
			  int tx_start, int tx_end, size_t offset)
{
	(void)dev; (void)rx_start; (void)rx_end;
	(void)tx_start; (void)tx_end; (void)offset;
	return 0;
}

void netdev_notify_peers(struct net_device *dev)
{
	/* A gratuitous ARP, so switches relearn the port after a migration.
	 * nk does not migrate and has nobody to tell. */
	(void)dev;
}

/* --- dynamic interrupt moderation ------------------------------------- */

/*
 * net_dim watches throughput and latency and retunes interrupt coalescing.
 * Answered as "no moderation": nk's traffic is one packet at a time and a
 * profile chosen from measurements it has never taken would be a guess with
 * a number on it.
 */
void net_dim(struct dim *dim, const struct dim_sample *end_sample)
{ (void)dim; (void)end_sample; }
int net_dim_init_irq_moder(struct net_device *dev, u8 profile_flags,
			   u8 coal_flags, u8 rx_mode, u8 tx_mode,
			   void (*rx_dim_work)(struct work_struct *),
			   void (*tx_dim_work)(struct work_struct *))
{
	(void)dev; (void)profile_flags; (void)coal_flags;
	(void)rx_mode; (void)tx_mode; (void)rx_dim_work; (void)tx_dim_work;
	return 0;
}
void net_dim_free_irq_moder(struct net_device *dev) { (void)dev; }
void net_dim_work_cancel(struct dim *dim) { (void)dim; }

/* --- byte queue limits -------------------------------------------------- */

/*
 * BQL keeps a transmit queue just full enough to stay busy and no fuller, so
 * that latency-sensitive traffic is not stuck behind a megabyte of buffered
 * bulk. It is a control loop over a queue nk does not have: nk submits one
 * frame and waits for it. So the accounting is kept honest -- num_completed
 * really does track what has been sent -- and the limit is left alone,
 * because nothing here would obey it.
 */
void dql_completed(struct dql *dql, unsigned int count)
{
	dql->num_completed += count;
}

void dql_reset(struct dql *dql)
{
	dql->num_queued = 0;
	dql->num_completed = 0;
	dql->last_obj_cnt = 0;
}
