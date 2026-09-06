// SPDX-License-Identifier: GPL-2.0
/*
 * The handful of string functions Linux expects the architecture to provide.
 *
 * Not taken from lib/string.c, and the reason is worth recording: on arm64
 * every one of these is `#ifndef __HAVE_ARCH_STRCMP`-ed out of the generic C
 * and supplied by hand-written assembly in arch/arm64/lib/. Pulling those in
 * would mean building .S files and, with them, the alternatives-patching
 * machinery they use to pick a variant at boot -- which nk deliberately does
 * not run.
 *
 * So these are written out plainly. They are the versions where nobody has
 * ever needed to be clever, and being slower than a hand-vectorised strcmp
 * costs nk nothing it can currently measure.
 *
 * memcpy, memset, memmove and memcmp are deliberately *not* here: Rust's
 * compiler_builtins already provides them for a bare-metal target, and a
 * second definition would be a duplicate symbol rather than a choice.
 */

#include <linux/string.h>
#include <linux/types.h>

int strcmp(const char *a, const char *b)
{
	while (*a && *a == *b) {
		a++;
		b++;
	}
	return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, __kernel_size_t n)
{
	while (n && *a && *a == *b) {
		a++;
		b++;
		n--;
	}
	return n ? (unsigned char)*a - (unsigned char)*b : 0;
}

char *strcpy(char *dst, const char *src)
{
	char *out = dst;

	while ((*dst++ = *src++))
		;
	return out;
}

__kernel_size_t strlen(const char *s)
{
	const char *p = s;

	while (*p)
		p++;
	return p - s;
}

char *strchr(const char *s, int c)
{
	for (; *s; s++)
		if (*s == (char)c)
			return (char *)s;
	return c ? NULL : (char *)s;
}

__kernel_size_t strspn(const char *s, const char *accept)
{
	const char *p;

	for (p = s; *p; p++)
		if (!strchr(accept, *p))
			break;
	return p - s;
}

__kernel_size_t strcspn(const char *s, const char *reject)
{
	const char *p;

	for (p = s; *p; p++)
		if (strchr(reject, *p))
			break;
	return p - s;
}
