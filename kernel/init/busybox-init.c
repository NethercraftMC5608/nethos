/* Start busybox, so nk's execve is what runs a program nobody wrote for nk.
 *
 * busybox is a good test precisely because it is indifferent to us: a real,
 * widely used, statically linked Linux binary that will use whatever syscalls
 * it needs and report an errno when one is missing.
 */
#include <unistd.h>

int main(int argc, char **argv)
{
	char *bb[] = { "busybox", "sh", "-c",
		       /* A shell, a pipeline of its own children, a file it
			* created, and output sent somewhere other than the
			* console. Redirection is the point: `> /tmp/out` is a
			* dup2 onto descriptor 1, and until Linux owned the
			* console nk answered descriptor 1 by its number and
			* the file would have stayed empty. */
		       "echo redirected > /tmp/out; "
		       "busybox ls -l /; "
		       "busybox cat /tmp/out",
		       NULL };
	char *envp[] = { "PATH=/bin", "HOME=/", NULL };

	(void)argc;
	(void)argv;
	execve("/bin/busybox", bb, envp);
	return 1;
}
