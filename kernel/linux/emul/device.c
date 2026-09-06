// SPDX-License-Identifier: GPL-2.0
/*
 * The driver model: buses, devices, drivers, and matching one to another.
 *
 * Linux's real device model is enormous -- kobjects, sysfs, klists, device
 * links, deferred probe, power management, uevents. Almost none of that is
 * what a driver needs in order to *work*; it is what a running system needs in
 * order to be introspected and managed. What virtio_mmio.c and virtio_blk.c
 * actually require of it is small and can be stated in a sentence:
 *
 *   a driver registers, a device appears, something notices they match, and
 *   the bus's probe is called with the device.
 *
 * That is what is here. Two lists and a matching loop.
 *
 * The order matters and is the reason initcall levels exist: the virtio bus
 * registers at level 4, virtio_blk's driver at 6, and nk presents the devices
 * later still. So every one of the three arrival orders has to work --
 * driver-then-device and device-then-driver both trigger the same match --
 * and that is why both device_add and driver_register end in the same walk.
 */

#include <linux/device.h>
#include <linux/device/bus.h>
#include <linux/list.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

static LIST_HEAD(devices);
static LIST_HEAD(drivers);

/*
 * Linux keeps a device's list membership inside `struct device_private`, which
 * is not a public type. Rather than fake one, each registered device and
 * driver gets a small node of our own pointing at it.
 */
struct node {
	struct list_head link;
	void *obj;
};

static void add(struct list_head *head, void *obj)
{
	struct node *n = nk_alloc(sizeof(*n), 8);

	if (!n)
		return;
	n->obj = obj;
	list_add_tail(&n->link, head);
}

static void del(struct list_head *head, void *obj)
{
	struct node *n, *tmp;

	list_for_each_entry_safe(n, tmp, head, link) {
		if (n->obj == obj) {
			list_del(&n->link);
			nk_free(n);
			return;
		}
	}
}

/*
 * Bind, if they match.
 *
 * `bus->probe` rather than `drv->probe` where the bus provides one: that is
 * how Linux does it, and it is load-bearing here. virtio's bus probe is
 * virtio_dev_probe, which negotiates the feature bits and sets the DRIVER_OK
 * status before ever calling the driver's own probe. Calling drv->probe
 * directly would hand virtio_blk a device the device itself does not yet
 * consider ready.
 */
static int try_bind(struct device *dev, struct device_driver *drv)
{
	int ret;

	if (dev->driver || dev->bus != drv->bus)
		return 0;
	if (drv->bus->match && !drv->bus->match(dev, drv))
		return 0;

	dev->driver = drv;
	ret = drv->bus->probe ? drv->bus->probe(dev) :
	      (drv->probe ? drv->probe(dev) : 0);
	if (ret) {
		/*
		 * Not an error worth reporting. Every one of the 32 virtio-mmio
		 * transports QEMU advertises is offered to the driver, and the
		 * ones with nothing behind them are rejected here, by the
		 * driver's own check of the device ID. That is the intended
		 * path, not a failure.
		 */
		dev->driver = NULL;
		return 0;
	}
	return 1;
}

static void match_all(void)
{
	struct node *dn, *rn;

	list_for_each_entry(dn, &devices, link)
		list_for_each_entry(rn, &drivers, link)
			if (try_bind(dn->obj, rn->obj))
				break;
}

int bus_register(const struct bus_type *bus)
{
	/*
	 * Nothing to do. Linux allocates the bus's subsys_private here -- the
	 * device and driver klists, the sysfs directory, the probe workqueue.
	 * The lists live in this file instead and are keyed on dev->bus, so
	 * registration is only an announcement.
	 */
	(void)bus;
	return 0;
}

void bus_unregister(const struct bus_type *bus)
{
	(void)bus;
}

int driver_register(struct device_driver *drv)
{
	add(&drivers, drv);
	match_all();
	return 0;
}

void driver_unregister(struct device_driver *drv)
{
	del(&drivers, drv);
}

void device_initialize(struct device *dev)
{
	dev->kobj.name = NULL;
	dev->driver = NULL;
	INIT_LIST_HEAD(&dev->devres_head);
}

int dev_set_name(struct device *dev, const char *fmt, ...)
{
	va_list args;
	char *name = nk_alloc(64, 8);

	if (!name)
		return -ENOMEM;
	va_start(args, fmt);
	vsnprintf(name, 64, fmt, args);
	va_end(args);
	dev->kobj.name = name;
	return 0;
}

int device_add(struct device *dev)
{
	add(&devices, dev);
	match_all();
	return 0;
}

void device_unregister(struct device *dev)
{
	del(&devices, dev);
}

void put_device(struct device *dev)
{
	/*
	 * No reference counting anywhere in nk. Devices are created at boot
	 * and never removed, so every count would be a number nothing reads.
	 * Hot-unplug is what would make this matter, and nk cannot do it.
	 */
	(void)dev;
}

struct device *get_device(struct device *dev)
{
	return dev;
}

const char *dev_driver_string(const struct device *dev)
{
	if (dev && dev->driver && dev->driver->name)
		return dev->driver->name;
	return "nk";
}
