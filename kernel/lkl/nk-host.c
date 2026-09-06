// SPDX-License-Identifier: GPL-2.0
/*
 * nk as a machine for Linux to run on.
 *
 * This is the nk equivalent of LKL's `tools/lkl/lib/posix-host.c`, which
 * presents a POSIX process as a machine. Here the machine is nk: its
 * scheduler provides the threads, its heap the memory, its timer the clock,
 * and its console the output.
 *
 * The whole of Linux arrives as one object. `arch/lkl` is a real, maintained
 * architecture port -- 5,168 lines against 26,636 for `arch/um` and 179,127
 * for `arch/arm64` -- whose hardware is this struct of function pointers.
 * Built for aarch64 it produces `lkl.o`, containing the VFS, ext4, the
 * network stack and every system call, with exactly five undefined symbols.
 *
 * Which is the point, and the difference from `kernel/linux/emul/`. That shim
 * reuses Linux's leaf code and hand-writes the kernel beneath it, so it grows
 * a little with every driver ported. This reuses all of Linux and writes only
 * the machine beneath *that*. It does not grow.
 */

#include "host_ops.h"

/* nk's side, from kernel/core/src/hostops.rs. Declared here rather than in a
 * header nk generates: the interface is small enough to state, and stating it
 * is what makes a mismatch a compile error. */
void nk_host_print(const char *s, int len);
void nk_host_panic(void);

struct nk_sem;
struct nk_mutex;
struct nk_sem *nk_sem_alloc(int count);
void nk_sem_free(struct nk_sem *s);
void nk_sem_up(struct nk_sem *s);
void nk_sem_down(struct nk_sem *s);
struct nk_mutex *nk_mutex_alloc(int recursive);
void nk_mutex_free(struct nk_mutex *m);
void nk_mutex_lock(struct nk_mutex *m);
void nk_mutex_unlock(struct nk_mutex *m);

unsigned long nk_thread_create(void (*f)(void *), void *arg);
unsigned long nk_thread_self(void);
void nk_thread_exit(void);
int nk_thread_join(unsigned long id);

unsigned long nk_tls_alloc(void);
void nk_tls_free(unsigned long key);
int nk_tls_set(unsigned long key, void *value);
void *nk_tls_get(unsigned long key);

void *nk_alloc(unsigned long size, unsigned long align);
void nk_free(void *p);
void *nk_alloc_pages(unsigned long n);

unsigned long long nk_time_ns(void);
unsigned long nk_timer_alloc(void (*fire)(void));
int nk_timer_set_oneshot(unsigned long id, unsigned long long delta_ns);
void nk_timer_free(unsigned long id);

int nk_setjmp(unsigned long *buf);
void nk_longjmp(unsigned long *buf, int val);

/* ------------------------------------------------------------------ */

static void host_print(const char *str, int len)
{
	nk_host_print(str, len);
}

static void host_panic(void)
{
	nk_host_panic();
}

static struct lkl_sem *host_sem_alloc(int count)
{
	return (struct lkl_sem *)nk_sem_alloc(count);
}

static void host_sem_free(struct lkl_sem *s) { nk_sem_free((struct nk_sem *)s); }
static void host_sem_up(struct lkl_sem *s)   { nk_sem_up((struct nk_sem *)s); }
static void host_sem_down(struct lkl_sem *s) { nk_sem_down((struct nk_sem *)s); }

static struct lkl_mutex *host_mutex_alloc(int recursive)
{
	return (struct lkl_mutex *)nk_mutex_alloc(recursive);
}

static void host_mutex_free(struct lkl_mutex *m)   { nk_mutex_free((struct nk_mutex *)m); }
static void host_mutex_lock(struct lkl_mutex *m)   { nk_mutex_lock((struct nk_mutex *)m); }
static void host_mutex_unlock(struct lkl_mutex *m) { nk_mutex_unlock((struct nk_mutex *)m); }

/*
 * Thread identifiers are nk's task slot plus one, and the plus one is
 * load-bearing.
 *
 * LKL uses zero as "nobody owns the CPU": lkl_cpu_get tests `if (cpu.owner &&
 * !thread_equal(cpu.owner, self))` before deciding whether the CPU is taken.
 * nk numbers its tasks from zero, so the boot task -- the one that calls
 * lkl_start_kernel and takes the CPU first -- was indistinguishable from no
 * owner at all. Every later acquisition then believed the CPU was free,
 * cpu.count went wrong, and the machine deadlocked with every thread blocked
 * and the clock never re-armed.
 *
 * Nothing reports a sentinel collision. It presents as a kernel that stops.
 */
static lkl_thread_t host_thread_create(void (*f)(void *), void *arg)
{
	unsigned long id = nk_thread_create(f, arg);

	return (lkl_thread_t)(id + 1);
}

static void host_thread_detach(void) { }
static void host_thread_exit(void) { nk_thread_exit(); }
static int host_thread_join(lkl_thread_t tid)
{
	return nk_thread_join((unsigned long)tid - 1);
}

static lkl_thread_t host_thread_self(void)
{
	return (lkl_thread_t)(nk_thread_self() + 1);
}
static int host_thread_equal(lkl_thread_t a, lkl_thread_t b) { return a == b; }

static struct lkl_tls_key *host_tls_alloc(void (*destructor)(void *))
{
	/* Destructors are not run. nk's threads are kernel threads that live
	 * for the life of the machine, so a key's value is never reclaimed --
	 * a leak bounded by the number of keys times the number of threads,
	 * and one that would matter the moment threads came and went. */
	(void)destructor;
	return (struct lkl_tls_key *)(nk_tls_alloc() + 1);
}

static void host_tls_free(struct lkl_tls_key *key)
{
	nk_tls_free((unsigned long)key - 1);
}

static int host_tls_set(struct lkl_tls_key *key, void *data)
{
	return nk_tls_set((unsigned long)key - 1, data);
}

static void *host_tls_get(struct lkl_tls_key *key)
{
	return nk_tls_get((unsigned long)key - 1);
}

/* ARCH_KMALLOC_MINALIGN's worth, because Linux assumes anything from the
 * allocator is safe to hand to a device. */
static void *host_mem_alloc(unsigned long size) { return nk_alloc(size, 128); }
static void host_mem_free(void *p) { nk_free(p); }

static void *host_page_alloc(unsigned long size)
{
	return nk_alloc_pages((size + 4095) / 4096);
}

static void host_page_free(void *addr, unsigned long size)
{
	/* Not freed: nk's frame allocator returns single pages and this is a
	 * run of them, so handing back the first would look correct and
	 * corrupt the free list. A buddy allocator is what fixes it. */
	(void)addr;
	(void)size;
}

static unsigned long long host_time(void) { return nk_time_ns(); }

static void *host_timer_alloc(void (*fn)(void))
{
	return (void *)(nk_timer_alloc(fn) + 1);
}

static int host_timer_set_oneshot(void *timer, unsigned long delta)
{
	return nk_timer_set_oneshot((unsigned long)timer - 1, delta);
}

static void host_timer_free(void *timer)
{
	nk_timer_free((unsigned long)timer - 1);
}

/* nk is identity mapped and everything below 1GB is already Device memory,
 * so a physical address is the pointer. See kernel/core/src/paging.rs. */
static void *host_ioremap(long addr, int size)
{
	(void)size;
	return (void *)addr;
}

static int host_iomem_access(const volatile void *addr, void *val, int size,
			     int write)
{
	/* Explicit widths, and never a memcpy: a device register has to be
	 * read and written in one access of exactly the right size, and the
	 * compiler is free to turn a byte loop into anything it likes. */
	switch (size) {
	case 1:
		if (write) *(volatile unsigned char *)addr = *(unsigned char *)val;
		else *(unsigned char *)val = *(volatile unsigned char *)addr;
		break;
	case 2:
		if (write) *(volatile unsigned short *)addr = *(unsigned short *)val;
		else *(unsigned short *)val = *(volatile unsigned short *)addr;
		break;
	case 4:
		if (write) *(volatile unsigned int *)addr = *(unsigned int *)val;
		else *(unsigned int *)val = *(volatile unsigned int *)addr;
		break;
	case 8:
		if (write) *(volatile unsigned long *)addr = *(unsigned long *)val;
		else *(unsigned long *)val = *(volatile unsigned long *)addr;
		break;
	default:
		return -1;
	}
	return 0;
}

static void host_jmp_buf_set(struct lkl_jmp_buf *jmpb, void (*f)(void))
{
	if (!nk_setjmp(jmpb->buf))
		f();
}

static void host_jmp_buf_longjmp(struct lkl_jmp_buf *jmpb, int val)
{
	nk_longjmp(jmpb->buf, val);
}

void *memcpy(void *dest, const void *src, unsigned long n);
void *memset(void *s, int c, unsigned long n);
void *memmove(void *dest, const void *src, unsigned long n);

struct lkl_host_operations lkl_host_ops = {
	.print = host_print,
	.panic = host_panic,

	.sem_alloc = host_sem_alloc,
	.sem_free = host_sem_free,
	.sem_up = host_sem_up,
	.sem_down = host_sem_down,

	.mutex_alloc = host_mutex_alloc,
	.mutex_free = host_mutex_free,
	.mutex_lock = host_mutex_lock,
	.mutex_unlock = host_mutex_unlock,

	.thread_create = host_thread_create,
	.thread_detach = host_thread_detach,
	.thread_exit = host_thread_exit,
	.thread_join = host_thread_join,
	.thread_self = host_thread_self,
	.thread_equal = host_thread_equal,

	.tls_alloc = host_tls_alloc,
	.tls_free = host_tls_free,
	.tls_set = host_tls_set,
	.tls_get = host_tls_get,

	.mem_alloc = host_mem_alloc,
	.mem_free = host_mem_free,
	.page_alloc = host_page_alloc,
	.page_free = host_page_free,

	.time = host_time,
	.timer_alloc = host_timer_alloc,
	.timer_set_oneshot = host_timer_set_oneshot,
	.timer_free = host_timer_free,

	.ioremap = host_ioremap,
	.iomem_access = host_iomem_access,

	.jmp_buf_set = host_jmp_buf_set,
	.jmp_buf_longjmp = host_jmp_buf_longjmp,

	.memcpy = memcpy,
	.memset = memset,
	.memmove = memmove,
};

/* Two of lkl.o's five undefined symbols. The other three are the compiler's
 * outline atomics. */
int lkl_printf(const char *fmt, ...);
int lkl_printf(const char *fmt, ...)
{
	/* No formatting: this is the path Linux uses when it cannot use its
	 * own printk -- before the kernel is up, or while it is failing --
	 * and pulling a vsnprintf in here would mean depending on the thing
	 * being reported on. The format string alone says which message it
	 * is. */
	const char *p = fmt;
	int n = 0;

	while (*p++)
		n++;
	nk_host_print(fmt, n);
	return n;
}

void lkl_bug(const char *fmt, ...);
void lkl_bug(const char *fmt, ...)
{
	lkl_printf(fmt);
	nk_host_panic();
}
