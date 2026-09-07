#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>

/* Syscall soak for the desktop: socketpair and SCM_RIGHTS for the Wayland
 * socket, epoll for the compositor loop, eventfd for thread wakeups, memfd
 * for wl_shm buffers, poll for everything that waits the other way.
 *
 * One PIE binary, stdlib only, in the shape of runtime.c: a marker per
 * facility, perror and a distinct exit code on failure. Everything here
 * forwards to Linux through syscall.rs -- nk owns none of these numbers --
 * except the memfd mmap, which is nk's sys_mmap and is expected to fail
 * until MAP_SHARED exists. That failure is the point of running it.
 */
static int check_socketpair(void) {
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) < 0) {
        perror("socketpair");
        return 1;
    }
    const char msg[4] = { 's', 'o', 'a', 'k' };
    if (send(sv[0], msg, sizeof msg, 0) != (ssize_t)sizeof msg) {
        perror("send");
        return 1;
    }
    char buf[4] = { 0 };
    if (recv(sv[1], buf, sizeof buf, MSG_WAITALL) != (ssize_t)sizeof buf ||
        memcmp(buf, msg, sizeof msg) != 0) {
        perror("recv");
        return 1;
    }
    close(sv[0]);
    close(sv[1]);
    puts("SOAK_SOCKETPAIR_OK");
    fflush(stdout);
    return 0;
}

static int check_scm_rights(void) {
    int p[2];
    if (pipe(p) < 0) { perror("pipe"); return 2; }
    int sp[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sp) < 0) {
        perror("socketpair scm"); return 2;
    }
    struct msghdr m;
    memset(&m, 0, sizeof m);
    char dummy = 'x';
    struct iovec io = { .iov_base = &dummy, .iov_len = 1 };
    m.msg_iov = &io;
    m.msg_iovlen = 1;
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    m.msg_control = cbuf;
    m.msg_controllen = sizeof cbuf;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &p[1], sizeof(int));
    if (sendmsg(sp[0], &m, 0) < 0) { perror("sendmsg"); return 2; }

    struct msghdr m2;
    memset(&m2, 0, sizeof m2);
    char dummy2 = 0;
    struct iovec io2 = { .iov_base = &dummy2, .iov_len = 1 };
    m2.msg_iov = &io2;
    m2.msg_iovlen = 1;
    char cbuf2[CMSG_SPACE(sizeof(int))];
    memset(cbuf2, 0, sizeof cbuf2);
    m2.msg_control = cbuf2;
    m2.msg_controllen = sizeof cbuf2;
    if (recvmsg(sp[1], &m2, 0) < 0) { perror("recvmsg"); return 2; }
    struct cmsghdr *c2 = CMSG_FIRSTHDR(&m2);
    if (!c2 || c2->cmsg_level != SOL_SOCKET || c2->cmsg_type != SCM_RIGHTS ||
        c2->cmsg_len != CMSG_LEN(sizeof(int))) {
        fprintf(stderr, "scm: no fd received\n");
        return 2;
    }
    int got = -1;
    memcpy(&got, CMSG_DATA(c2), sizeof(int));
    if (write(got, "y", 1) != 1) { perror("write via passed fd"); return 2; }
    char r = 0;
    if (read(p[0], &r, 1) != 1 || r != 'y') {
        perror("read via original pipe"); return 2;
    }
    close(p[0]); close(p[1]); close(got);
    close(sp[0]); close(sp[1]);
    puts("SOAK_SCM_RIGHTS_OK");
    fflush(stdout);
    return 0;
}

static int check_epoll(void) {
    int pp[2];
    if (pipe(pp) < 0) { perror("pipe epoll"); return 3; }
    int efd = epoll_create1(EPOLL_CLOEXEC);
    if (efd < 0) efd = epoll_create1(0);
    if (efd < 0) { perror("epoll_create1"); return 3; }
    struct epoll_event ev;
    memset(&ev, 0, sizeof ev);
    ev.events = EPOLLIN;
    ev.data.fd = pp[0];
    if (epoll_ctl(efd, EPOLL_CTL_ADD, pp[0], &ev) < 0) {
        perror("epoll_ctl"); return 3;
    }
    if (write(pp[1], "e", 1) != 1) { perror("write epoll pipe"); return 3; }
    struct epoll_event out[1];
    int n = epoll_wait(efd, out, 1, 2000);
    if (n != 1 || out[0].data.fd != pp[0] || !(out[0].events & EPOLLIN)) {
        fprintf(stderr, "epoll_wait: n=%d\n", n);
        return 3;
    }
    close(pp[0]); close(pp[1]); close(efd);
    puts("SOAK_EPOLL_OK");
    fflush(stdout);
    return 0;
}

static int check_eventfd(void) {
    int efd = eventfd(0, EFD_CLOEXEC);
    if (efd < 0) efd = eventfd(0, 0);
    if (efd < 0) { perror("eventfd"); return 4; }
    uint64_t w = 5, r = 0;
    if (write(efd, &w, sizeof w) != (ssize_t)sizeof w) {
        perror("eventfd write"); return 4;
    }
    if (read(efd, &r, sizeof r) != (ssize_t)sizeof r || r != 5) {
        perror("eventfd read"); return 4;
    }
    close(efd);
    puts("SOAK_EVENTFD_OK");
    fflush(stdout);
    return 0;
}

static int check_memfd(void) {
    int mfd = memfd_create("soak", MFD_CLOEXEC);
    if (mfd < 0) { perror("memfd_create"); return 5; }
    if (ftruncate(mfd, 4096) < 0) { perror("ftruncate"); return 5; }
    /* wl_shm needs MAP_SHARED. mmap is nk-owned rather than forwarded,
     * so a failure here names nk's missing MAP_SHARED, not Linux. */
    void *mp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0);
    if (mp == MAP_FAILED) { perror("mmap memfd"); return 5; }
    memcpy(mp, "wl_shm", 7);
    if (memcmp(mp, "wl_shm", 7) != 0) {
        fprintf(stderr, "memfd: readback mismatch\n");
        return 5;
    }
    if (munmap(mp, 4096) < 0) { perror("munmap"); return 5; }
    close(mfd);
    puts("SOAK_MEMFD_OK");
    fflush(stdout);
    return 0;
}

static int check_poll(void) {
    int pp[2];
    if (pipe(pp) < 0) { perror("pipe poll"); return 6; }
    if (write(pp[1], "p", 1) != 1) { perror("write poll pipe"); return 6; }
    struct pollfd pfd;
    pfd.fd = pp[0];
    pfd.events = POLLIN;
    pfd.revents = 0;
    int n = poll(&pfd, 1, 2000);
    if (n != 1 || !(pfd.revents & POLLIN)) {
        fprintf(stderr, "poll: n=%d revents=%d\n", n, pfd.revents);
        return 6;
    }
    char c = 0;
    if (read(pp[0], &c, 1) != 1 || c != 'p') {
        perror("read poll pipe"); return 6;
    }
    close(pp[0]); close(pp[1]);
    puts("SOAK_POLL_OK");
    fflush(stdout);
    return 0;
}

int main(void) {
    int rc;
    if ((rc = check_socketpair())) return rc;
    if ((rc = check_scm_rights())) return rc;
    if ((rc = check_epoll())) return rc;
    if ((rc = check_eventfd())) return rc;
    if ((rc = check_memfd())) return rc;
    if ((rc = check_poll())) return rc;
    return 0;
}
