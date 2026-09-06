// SPDX-License-Identifier: GPL-2.0
/*
 * Device-tree properties, for devices that do not have a device-tree node.
 *
 * nk parses the device tree itself, in Rust, before any of this exists, and
 * hands the drivers only what it found -- a base address and an interrupt.
 * The platform devices it builds therefore carry no `of_node`, and every
 * function here is asked about a NULL one.
 *
 * That is a deliberate limit rather than an oversight. Wiring nk's parsed
 * tree back into `struct device_node` would mean building Linux's whole
 * of_node graph -- phandles, parents, aliases -- so that drivers could
 * re-parse what nk has already read. The drivers that matter here ask only
 * for optional hints, and the honest answer to every one is "not specified",
 * which is exactly what a missing property means.
 *
 * A driver that genuinely requires a property will fail its probe with a
 * clear message rather than misbehave, which is the right failure.
 */

#include <linux/of.h>
#include <linux/of_device.h>

bool of_property_read_bool(const struct device_node *np, const char *propname)
{
	(void)np;
	(void)propname;
	return false;
}

int of_device_is_compatible(const struct device_node *device,
			    const char *compat)
{
	(void)device;
	(void)compat;
	return 0;
}

struct device_node *of_get_next_available_child(const struct device_node *node,
						struct device_node *prev)
{
	(void)node;
	(void)prev;
	return NULL;
}

void of_node_put(struct device_node *node)
{
	(void)node;
}
