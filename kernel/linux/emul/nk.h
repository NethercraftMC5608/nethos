/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The whole of what the shim may call in nk.
 *
 * Deliberately small, and deliberately in terms of nothing but integers and
 * void pointers. nk knows none of Linux's types and must not start to; this
 * header is the entire vocabulary in which the two halves speak.
 */
#ifndef _NK_H
#define _NK_H

void nk_console_write(const char *s, unsigned long len);
void nk_halt(void);

void *nk_alloc(unsigned long size, unsigned long align);
void nk_free(void *p);
void *nk_alloc_pages(unsigned long n);

unsigned long long nk_irq_save(void);
void nk_irq_restore(unsigned long long flags);

void nk_yield(void);
unsigned long long nk_ticks(void);
unsigned long long nk_hz(void);

int nk_request_irq(unsigned int intid);

/* Told once, when a disk reports its size. */
void nk_set_capacity(unsigned long long sectors);

/* The alignment kmalloc is assumed to give. Linux guarantees ARCH_KMALLOC_
 * MINALIGN, and drivers quietly rely on it for DMA-able allocations. */
#define NK_KMALLOC_ALIGN 128

#endif /* _NK_H */
