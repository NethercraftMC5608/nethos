/* Fork/exit churn through Linux's CPU handover, minimally.
 *
 * The residual weston stall's shape is always a forked task blocked on
 * semaphore 3 (LKL's CPU semaphore) with downs one ahead of ups: a missing
 * hand-over, not a lost wakeup. weston is the only workload with concurrent
 * processes coming and going, so this churns fork+exit in a loop and checks
 * the kernel still answers a timed poll afterwards. No disk, no Python,
 * seconds per iteration -- the failure, if it reproduces, is nk's fork/exit
 * path through LKL's cpu.sem accounting, not weston.
 *
 * Why fork and not threads: exitpoll.c already proves a pthread that runs
 * and exits leaves poll working. Threads never touch new_host_task,
 * thread_sched_jb, or the TLS destructor path (del_host_task ->
 * lkl_cpu_get/put); forked processes touch all three, on both creation
 * (attach_process -> new_host_task) and exit (tls_cleanup -> del_host_task
 * -> lkl_cpu_get -> switch_to_host_task -> ... -> lkl_cpu_put).
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <poll.h>
#include <time.h>
#include <unistd.h>

#define NCHILD 25
#define NGEN 6

/* A poll that must expire. Returns the seconds it took, or -1. */
static double timed_poll(int ms)
{
	int fds[2];
	struct timespec a, b;
	struct pollfd p;
	int rc;

	if (pipe(fds) != 0)
		return -1;
	p.fd = fds[0];
	p.events = POLLIN;
	clock_gettime(CLOCK_MONOTONIC, &a);
	rc = poll(&p, 1, ms);
	clock_gettime(CLOCK_MONOTONIC, &b);
	close(fds[0]);
	close(fds[1]);
	if (rc != 0)
		return -1;
	return (b.tv_sec - a.tv_sec) + (b.tv_nsec - a.tv_nsec) / 1e9;
}

int main(void)
{
	double waited;
	int i;

	setvbuf(stdout, NULL, _IONBF, 0);

	/* Generations of children that overlap in time: at any moment several
	 * Linux tasks exist at once and exits interleave with creates, which
	 * is the weston shape (concurrent processes coming and going) that
	 * serial fork-reap never produces. */
	for (int g = 0; g < NGEN; g++) {
		pid_t kids[NCHILD];
		for (i = 0; i < NCHILD; i++) {
			pid_t c = fork();
			if (c < 0) {
				printf("FORKCHURN_FAIL fork g%d %d\n", g, i);
				return 1;
			}
			if (c == 0) {
				/* stagger exits so they land while siblings
				 * are still being created */
				usleep((i % 5) * 20000);
				_exit(i & 0xff);
			}
			kids[i] = c;
		}
		for (i = 0; i < NCHILD; i++) {
			int status;
			pid_t seen = wait4(kids[i], &status, 0, NULL);
			if (seen != kids[i] || !WIFEXITED(status) ||
			    WEXITSTATUS(status) != (i & 0xff)) {
				printf("FORKCHURN_FAIL reap g%d %d seen=%d status=%d\n",
				       g, i, (int)seen, status);
				return 2;
			}
		}
		printf("FORKCHURN_OK  generation %d: reaped %d children\n", g, NCHILD);
	}

	waited = timed_poll(1000);
	if (waited < 0) {
		printf("FORKCHURN_FAIL poll-after-churn\n");
		return 3;
	}
	printf("FORKCHURN_OK  poll expired after %.2fs\n", waited);
	printf("FORKCHURN_DONE\n");
	return 0;
}
