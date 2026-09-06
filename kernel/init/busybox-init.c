/* Start busybox, so nk's execve is what runs a program nobody wrote for nk.
 *
 * busybox is a good test precisely because it is indifferent to us: a real,
 * widely used, statically linked Linux binary that will use whatever syscalls
 * it needs and report an errno when one is missing.
 */
#include <unistd.h>

int main(int argc, char **argv)
{
	char *bb[] = { "busybox", "ls", "-l", "/", NULL };
	char *envp[] = { "PATH=/bin", "HOME=/", NULL };

	(void)argc;
	(void)argv;
	execve("/bin/busybox", bb, envp);
	return 1;
}
