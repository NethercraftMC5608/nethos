// SPDX-License-Identifier: GPL-2.0
/*
 * The DMA API, for a machine with no IOMMU and an identity map.
 *
 * A device address, a physical address and a virtual address are all the same
 * number here, so every mapping function is arithmetic and every sync is
 * nothing: the CPU's view and the device's view of memory cannot diverge when
 * there is no translation between them and the pages are coherent.
 *
 * Most of this is expected never to run. virtio_ring only reaches for the DMA
 * API when the device advertises VIRTIO_F_ACCESS_PLATFORM, which QEMU does
 * not do unless asked (`iommu_platform=on`); without it, virtio_ring uses
 * virt_to_phys directly and none of the map/unmap functions below are called.
 * They are written anyway because the alternative is a stub that halts the
 * machine the first time someone passes that option.
 */

#include <linux/dma-mapping.h>
#include <linux/scatterlist.h>

#include "nk.h"

int dma_set_mask(struct device *dev, u64 mask)
{
	if (dev->dma_mask)
		*dev->dma_mask = mask;
	return 0;
}

int dma_set_coherent_mask(struct device *dev, u64 mask)
{
	dev->coherent_dma_mask = mask;
	return 0;
}

/* No bounce buffer and no window, so no ceiling below the address space. */
size_t dma_max_mapping_size(struct device *dev)
{
	(void)dev;
	return SIZE_MAX;
}

void *dma_alloc_attrs(struct device *dev, size_t size, dma_addr_t *dma_handle,
		      gfp_t gfp, unsigned long attrs)
{
	unsigned long pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
	void *p;

	(void)dev;
	(void)gfp;
	(void)attrs;
	p = nk_alloc_pages(pages);
	if (!p)
		return NULL;
	*dma_handle = (dma_addr_t)(unsigned long)p;
	return p;
}

void dma_free_attrs(struct device *dev, size_t size, void *cpu_addr,
		    dma_addr_t dma_handle, unsigned long attrs)
{
	/* See free_pages_exact in slab.c: a run of frames, freed one at a
	 * time, would look correct and be wrong. */
	(void)dev; (void)size; (void)cpu_addr; (void)dma_handle; (void)attrs;
}

dma_addr_t dma_map_page_attrs(struct device *dev, struct page *page,
			      size_t offset, size_t size,
			      enum dma_data_direction dir, unsigned long attrs)
{
	(void)dev; (void)size; (void)dir; (void)attrs;
	return page_to_phys(page) + offset;
}

void dma_unmap_page_attrs(struct device *dev, dma_addr_t addr, size_t size,
			  enum dma_data_direction dir, unsigned long attrs)
{
	(void)dev; (void)addr; (void)size; (void)dir; (void)attrs;
}

/*
 * Nothing to sync. The pages are mapped Normal, Inner Shareable, write-back
 * by paging.rs, and QEMU's virtio devices read memory coherently; there is no
 * non-coherent DMA master on this machine to flush for. On real hardware with
 * a non-coherent device this is the first thing that would have to become
 * real, and it would present as data that is correct in memory and stale at
 * the device.
 */
bool __dma_need_sync(struct device *dev, dma_addr_t addr)
{
	(void)dev; (void)addr;
	return false;
}

void __dma_sync_single_for_cpu(struct device *dev, dma_addr_t addr, size_t size,
			       enum dma_data_direction dir)
{
	(void)dev; (void)addr; (void)size; (void)dir;
}

void __dma_sync_single_for_device(struct device *dev, dma_addr_t addr,
				  size_t size, enum dma_data_direction dir)
{
	(void)dev; (void)addr; (void)size; (void)dir;
}
