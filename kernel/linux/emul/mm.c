// SPDX-License-Identifier: GPL-2.0
/*
 * Two globals that make Linux's address arithmetic agree with nk's identity
 * map, and one observation that removed an entire subsystem's worth of work.
 *
 * arm64 converts a virtual address to a physical one like this:
 *
 *   __is_lm_address(x) ? __lm_to_phys(x) : __kimg_to_phys(x)
 *   __lm_to_phys(x)   = (x & ~PAGE_OFFSET) + PHYS_OFFSET   [PHYS_OFFSET = memstart_addr]
 *   __kimg_to_phys(x) = x - kimage_voffset
 *
 * nk's addresses are low -- 0x40000000 and up, identity mapped -- so
 * __is_lm_address is false for every one of them and everything goes through
 * __kimg_to_phys. With kimage_voffset at zero that is `x - 0`, and
 * virt_to_phys becomes the identity function it should be.
 *
 * The same fact makes struct page work, which is the part that mattered:
 *
 *   virt_to_page(x)   = vmemmap + (virt_to_phys(x) >> PAGE_SHIFT)
 *   page_to_phys(p)   = (p - vmemmap) << PAGE_SHIFT
 *   vmemmap           = (struct page *)VMEMMAP_START - (memstart_addr >> PAGE_SHIFT)
 *
 * With memstart_addr at zero these round-trip exactly: page_to_phys(
 * virt_to_page(x)) == x & PAGE_MASK. The `struct page *` in between is a
 * pointer into a vmemmap nk never allocated and never maps -- and nothing
 * dereferences it. virtio only ever converts it back.
 *
 * So nk needs no mem_map, no vmemmap, and no struct page array. Building one
 * would have cost 8MB on this guest and a page-table region to hold it.
 * Setting two globals to zero does instead. The limit is exact and worth
 * knowing: the moment something *dereferences* a struct page -- reads a page
 * flag, takes a reference, follows a mapping -- it faults on an address that
 * is not mapped, and that is when the real vmemmap has to be built.
 */

#include <linux/mm.h>
#include <linux/types.h>

/*
 * PAGE_OFFSET, not the real start of RAM and not zero. See above: this is the
 * value that makes virt_to_page and page_to_pfn inverses of each other for an
 * identity-mapped kernel. It also appears in __lm_to_phys, which nk never
 * reaches -- __is_lm_address is false for every address nk uses.
 */
s64 memstart_addr __read_mostly = PAGE_OFFSET;

/* Zero: virtual equals physical. */
u64 kimage_voffset __read_mostly;

/*
 * nk has no vmalloc area at all, so no address is ever in it. virtio_ring
 * asks in order to warn about a buffer it could not map with virt_to_phys --
 * a check that is exactly right, and that nk always passes.
 */
bool is_vmalloc_addr(const void *x)
{
	(void)x;
	return false;
}
