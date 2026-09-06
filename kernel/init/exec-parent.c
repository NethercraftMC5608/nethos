/* execve: replace this program with another, in the same process.
 *
 * The point of the test is that the second program's argv and envp were
 * *this* program's memory, in an address space that no longer exists by the
 * time it reads them. They have to be copied out before the replacement is
 * built, which is the part of execve a kernel gets wrong first.
 */
#include <stdio.h>
#include <unistd.h>
#include <errno.h>

int main(void)
{
	char *argv[] = { "/bin/second", "and", "its arguments", NULL };
	char *envp[] = { "NK=the environment survived too", NULL };

	/* A failed execve has to leave the caller with everything it had.
	 * That is the whole reason nk validates the new image and builds the
	 * new address space before it tears down the old one. */
	if (execve("/bin/there-is-no-such-file", argv, envp) != -1 ||
	    errno != ENOENT) {
		printf("parent: a failed execve did not report ENOENT\n");
		return 4;
	}
	printf("parent: survived a failed execve, errno was ENOENT\n");

	printf("parent: about to replace itself\n");
	fflush(stdout);
	execve("/bin/second", argv, envp);
	/* Only reached if execve failed, which is the promise: the caller
	 * still has everything it had. */
	perror("parent: execve");
	return 3;
}
