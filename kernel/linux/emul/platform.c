// SPDX-License-Identifier: GPL-2.0
/*
 * The platform bus, and the virtio-mmio transports nk finds on it.
 *
 * virtio_mmio.c is a platform driver: it does not look for anything itself,
 * it waits to be handed a `platform_device` carrying a memory resource and an
 * interrupt. Linux builds those from the device tree in drivers/of/platform.c,
 * which is a large file with a general problem to solve. nk has already
 * parsed the device tree for its own purposes, so it hands the two numbers
 * across the boundary and this file builds the device.
 *
 * Every transport is offered, including the empty ones. QEMU's `virt` always
 * advertises 32 and populates only those with a `-device` behind them; the
 * rest report a device ID of zero and virtio_mmio_probe rejects them itself,
 * by its own check, which is exactly the path that should run. Filtering them
 * out here would mean reimplementing that check against the driver, and the
 * two would eventually disagree.
 */

#include <linux/device.h>
#include <linux/dma-mapping.h>
#include <linux/io.h>
#include <linux/ioport.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

/*
 * Matching by name. Linux's platform bus tries the OF table, the ACPI table,
 * the id_table and then the name; nk creates the devices itself and names
 * them to match, so the last rule is the only one that can fire and the
 * others would be untested code.
 */
static int platform_match(struct device *dev, const struct device_driver *drv)
{
	struct platform_device *pdev = to_platform_device(dev);

	return strcmp(pdev->name, drv->name) == 0;
}

static int platform_probe(struct device *dev)
{
	struct platform_driver *drv = to_platform_driver(dev->driver);

	return drv->probe ? drv->probe(to_platform_device(dev)) : 0;
}

static const struct bus_type nk_platform_bus = {
	.name = "platform",
	.match = platform_match,
	.probe = platform_probe,
};

int __platform_driver_register(struct platform_driver *drv, struct module *owner,
			       const char *mod_name)
{
	(void)owner;
	(void)mod_name;
	drv->driver.bus = &nk_platform_bus;
	return driver_register(&drv->driver);
}

void platform_driver_unregister(struct platform_driver *drv)
{
	driver_unregister(&drv->driver);
}

/*
 * The lookup every other accessor here is built on. A real function in
 * Linux's drivers/base/platform.c rather than an inline, so implementing the
 * platform bus without it links only until something calls one of them.
 */
struct resource *platform_get_resource(struct platform_device *pdev,
				       unsigned int type, unsigned int num)
{
	unsigned int i;

	for (i = 0; i < pdev->num_resources; i++) {
		struct resource *r = &pdev->resource[i];

		if (resource_type(r) == type && num-- == 0)
			return r;
	}
	return NULL;
}

int platform_get_irq(struct platform_device *pdev, unsigned int num)
{
	struct resource *r = platform_get_resource(pdev, IORESOURCE_IRQ, num);

	return r ? (int)r->start : -ENXIO;
}

/*
 * "ioremap" with the MMU identity mapped and everything below 1GB already
 * mapped as Device memory by paging.rs: the physical address *is* the
 * pointer. The devm_ half is equally short -- nk never unbinds a driver, so
 * there is nothing to undo and no devres list to keep.
 */
void __iomem *devm_platform_ioremap_resource(struct platform_device *pdev,
					     unsigned int index)
{
	struct resource *r = platform_get_resource(pdev, IORESOURCE_MEM, index);

	if (!r)
		return IOMEM_ERR_PTR(-EINVAL);
	return (void __iomem *)(unsigned long)r->start;
}

/*
 * Called by nk once per virtio-mmio node in the device tree.
 *
 * Returns 0 whether or not anything binds. A transport with no device behind
 * it is not a failure -- see the note at the top of this file.
 */
int nk_add_virtio_mmio(unsigned long long base, unsigned long long size,
		       unsigned int irq);
int nk_add_virtio_mmio(unsigned long long base, unsigned long long size,
		       unsigned int irq)
{
	static int index;
	struct platform_device *pdev;
	struct resource *res;

	pdev = nk_alloc(sizeof(*pdev), 8);
	res = nk_alloc(sizeof(*res) * 2, 8);
	if (!pdev || !res)
		return -ENOMEM;
	memset(pdev, 0, sizeof(*pdev));
	memset(res, 0, sizeof(*res) * 2);

	res[0].start = base;
	res[0].end = base + size - 1;
	res[0].flags = IORESOURCE_MEM;
	res[1].start = irq;
	res[1].end = irq;
	res[1].flags = IORESOURCE_IRQ;

	pdev->name = "virtio-mmio";
	pdev->id = index++;
	pdev->num_resources = 2;
	pdev->resource = res;

	device_initialize(&pdev->dev);
	pdev->dev.bus = &nk_platform_bus;
	/*
	 * dma_set_mask writes through this pointer, so it has to point
	 * somewhere. Linux's platform_device_alloc aims it at a field in the
	 * platform_device for exactly the same reason.
	 */
	pdev->dev.dma_mask = &pdev->platform_dma_mask;
	pdev->dev.coherent_dma_mask = DMA_BIT_MASK(64);
	dev_set_name(&pdev->dev, "virtio-mmio.%d", pdev->id);

	return device_add(&pdev->dev);
}
