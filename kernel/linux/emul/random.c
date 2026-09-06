// SPDX-License-Identifier: GPL-2.0
/*
 * Randomness, of a kind. Read the warning before using this for anything.
 *
 * **This is not cryptographically secure and must never be treated as if it
 * were.** It is a 64-bit xorshift seeded from the architected counter. An
 * attacker who can observe one output can predict every subsequent one, and
 * the seed is a timer value with a few bits of real entropy in it at best.
 *
 * It exists because virtio_blk asks for randomness to pick a disk's name, and
 * halting the machine over a disk name would be absurd. Every other current
 * caller is of the same kind. nk has no entropy source: no RNG driver, no
 * jitter collection, no seed from the bootloader. When it grows one -- the
 * machine already has a virtio-rng device sitting unclaimed on the bus, which
 * is the obvious answer -- this file is what it replaces.
 *
 * Until then, anything here that reaches a security decision is a bug in the
 * caller, and there is no way for the caller to tell.
 */

#include <linux/random.h>
#include <linux/types.h>

static u64 state;

static u64 next(void)
{
	if (!state) {
		/* The virtual counter, which is at least not the same on every
		 * boot. It is not entropy and is not claimed to be. */
		u64 seed;

		asm volatile("mrs %0, cntvct_el0" : "=r"(seed));
		state = seed | 1;
	}
	state ^= state << 13;
	state ^= state >> 7;
	state ^= state << 17;
	return state;
}

void get_random_bytes(void *buf, size_t len)
{
	u8 *p = buf;

	while (len) {
		u64 v = next();
		size_t n = len < sizeof(v) ? len : sizeof(v);

		__builtin_memcpy(p, &v, n);
		p += n;
		len -= n;
	}
}

u8 get_random_u8(void)
{
	return (u8)next();
}

u16 get_random_u16(void)
{
	return (u16)next();
}

u32 get_random_u32(void)
{
	return (u32)next();
}

u64 get_random_u64(void)
{
	return next();
}
