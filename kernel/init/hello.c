/* The first program compiled by a real toolchain to run on nk.
 *
 * Ordinary C, ordinary glibc, built with `gcc -static -O2` and not modified
 * in any way for nk. That is the whole point of it: it was compiled against
 * Linux's syscall ABI by a compiler that has never heard of this kernel, and
 * it runs because nk answers the same numbers with the same meanings.
 *
 * It calls `_exit` rather than returning from main, and that is a limitation
 * of nk's rather than a choice: glibc's `exit` walks its atexit handlers and
 * tears stdio down, and somewhere in there control reaches `_start` again.
 * See docs/KERNEL.md, "What a real binary still cannot do".
 *
 * Built by kernel/ldk's container:
 *   docker run --rm -v "$PWD:/w" -w /w nethos-ldk \
 *       gcc -static -O2 -o hello kernel/init/hello.c
 * and run with:
 *   scripts/run-kernel.sh --lkl --init hello
 */
#include <unistd.h>

int main(int argc, char **argv)
{
	write(1, "hello from a real compiled binary, on nk\n", 41);
	/* argv[0] proves the initial stack is the shape a libc expects: this
	 * pointer was read off the stack nk built, not passed in a register. */
	write(1, argv[0], 8);
	write(1, "\n", 1);
	_exit(argc == 1 ? 7 : 1);
}
