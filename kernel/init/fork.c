/* fork, wait4, and execve together: what a shell is made of.
 *
 * The child is a copy of this process at this instruction, with its own
 * address space -- writing to a variable here must not be visible there, and
 * the test is that both sides read back what they themselves wrote.
 */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/wait.h>

static int shared = 1;

int main(void)
{
	pid_t child, seen;
	int status;

	child = fork();
	if (child < 0) {
		perror("fork");
		return 1;
	}

	if (child == 0) {
		shared = 20;
		printf("child: fork() returned 0, shared is %d\n", shared);
		fflush(stdout);
		_exit(9);
	}

	shared = 10;
	printf("parent: fork() returned a pid, shared is %d\n", shared);

	seen = wait4(child, &status, 0, NULL);
	if (seen != child) {
		printf("parent: wait4 returned the wrong pid\n");
		return 2;
	}
	if (!WIFEXITED(status)) {
		printf("parent: child did not exit normally\n");
		return 3;
	}
	printf("parent: reaped its child, which exited %d\n", WEXITSTATUS(status));

	/* And with no children left, waiting must say so rather than block. */
	if (wait4(-1, &status, 0, NULL) != -1) {
		printf("parent: wait4 with no children did not fail\n");
		return 4;
	}
	printf("parent: wait4 with no children returned ECHILD\n");
	return 7;
}
