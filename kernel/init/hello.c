/* The first program compiled by a real toolchain to run on nk.
 *
 * Ordinary C, ordinary glibc, built with `gcc -static -O2` and not modified
 * in any way for nk. That is the whole point of it: it was compiled against
 * Linux's syscall ABI by a compiler that has never heard of this kernel, and
 * it runs because nk answers the same numbers with the same meanings.
 *
 * It deliberately does the things that are easy for a kernel to get subtly
 * wrong. printf drags in malloc, stdio buffering and an fstat on the console.
 * argv[0] can only be read off the stack nk built, not passed in a register.
 * A destructor and returning from main both go through glibc's exit path,
 * which walks its atexit handlers -- including the one it takes from x0 at
 * process entry, which has to be zero.
 *
 * Built by kernel/ldk's container:
 *   docker run --rm -v "$PWD:/w" -w /w nethos-ldk \
 *       gcc -static -O2 -o build/nk-hello kernel/init/hello.c
 * and run with:
 *   scripts/run-kernel.sh --lkl --init build/nk-hello
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void __attribute__((destructor)) goodbye(void)
{
	printf("and its destructor ran on the way out.\n");
	fflush(stdout);
}

int main(int argc, char **argv)
{
	char *heap = malloc(64);

	if (heap == NULL)
		return 1;
	strcpy(heap, "the heap works too");
	printf("hello from a real compiled binary, on nk.\n");
	printf("  argv[0] is %s, argc is %d, and %s.\n", argv[0], argc, heap);
	free(heap);
	return argc == 1 ? 7 : 1;
}
