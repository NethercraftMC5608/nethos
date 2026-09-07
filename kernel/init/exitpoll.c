/* The thread-exit hang, in as little as it takes to show it.
 *
 * The original sighting was Python on a 35MB ext4 disk, which is minutes per
 * iteration and half a dozen moving parts. This is the same failure with
 * none of them: a thread that runs and returns, and then a poll with a
 * timeout that never expires.
 *
 * The distinction that matters, and that the first diagnosis got backwards:
 * a thread merely *blocked* is harmless -- step 1 proves the kernel still
 * answers a timed poll while another thread sits in read() forever. It is
 * the thread *exiting* that breaks it. So the two steps run in that order
 * and the second is the one that hangs.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <poll.h>
#include <pthread.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static int blocked_pipe[2];

static void *sits_forever(void *arg)
{
	char c;
	(void)arg;
	read(blocked_pipe[0], &c, 1);   /* nothing is ever written */
	return NULL;
}

static void *returns_at_once(void *arg)
{
	(void)arg;
	return NULL;                    /* the whole point: it exits */
}

/* A poll that must expire. Returns the seconds it actually took, or -1. */
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
	pthread_t t;
	double waited;

	setvbuf(stdout, NULL, _IONBF, 0);

	if (pipe(blocked_pipe) != 0) {
		printf("EXITPOLL_FAIL pipe\n");
		return 1;
	}

	/* 1. A blocked thread must not stop the kernel answering. */
	pthread_create(&t, NULL, sits_forever, NULL);
	usleep(200000);
	waited = timed_poll(1000);
	if (waited < 0) {
		printf("EXITPOLL_FAIL poll-with-blocked-thread\n");
		return 2;
	}
	printf("EXITPOLL_OK   blocked-thread    poll expired after %.2fs\n", waited);

	/* 2. The same call, after a thread has exited. This is the bug. */
	pthread_create(&t, NULL, returns_at_once, NULL);
	pthread_join(t, NULL);
	printf("  a thread has now run and exited\n");

	waited = timed_poll(1000);
	if (waited < 0) {
		printf("EXITPOLL_FAIL poll-after-thread-exit\n");
		return 3;
	}
	printf("EXITPOLL_OK   exited-thread     poll expired after %.2fs\n", waited);

	printf("EXITPOLL_DONE\n");
	return 0;
}
