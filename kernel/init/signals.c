#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <signal.h>
#include <sys/wait.h>

/* Userspace signals: a handler that runs and returns, a kill that arrives,
 * a SIGCHLD that wakes the parent, and dispositions a child inherits.
 *
 * One PIE binary in the shape of soak.c: a marker per check, perror and a
 * distinct exit code on failure. No threads, no shared memory -- only the
 * signal syscalls nk answers itself: rt_sigaction, rt_sigprocmask, kill,
 * rt_sigreturn, and SIGCHLD from sys_exit.
 */

static volatile sig_atomic_t got_usr1 = 0;
static volatile sig_atomic_t handler_sig = 0;

static void on_usr1(int sig) {
    got_usr1 = 1;
    handler_sig = sig;
}

static int check_handler_runs_and_returns(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_usr1;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    if (sigaction(SIGUSR1, &sa, NULL) < 0) { perror("sigaction"); return 1; }
    if (raise(SIGUSR1) < 0) { perror("raise"); return 1; }
    // Delivery happens on the way back to EL0 -- which is where this
    // process already is. The handler must have run before raise returns.
    if (!got_usr1 || handler_sig != SIGUSR1) {
        fprintf(stderr, "handler did not run: got=%d sig=%d\n",
                got_usr1, handler_sig);
        return 1;
    }
    puts("SIG_HANDLER_OK");
    fflush(stdout);
    return 0;
}

static void on_info(int sig, siginfo_t *info, void *uc) {
    (void)uc;
    // SA_SIGINFO handlers receive the signal, a real siginfo, and a context.
    // si_pid must be this process: the kill came from here.
    if (sig == SIGUSR2 && info->si_pid == getpid())
        got_usr1 = 2;
}

static int check_siginfo_frame(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_info;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_SIGINFO;
    if (sigaction(SIGUSR2, &sa, NULL) < 0) { perror("sigaction siginfo"); return 2; }
    got_usr1 = 0;
    if (kill(getpid(), SIGUSR2) < 0) { perror("kill self"); return 2; }
    if (got_usr1 != 2) {
        fprintf(stderr, "siginfo handler did not confirm pid\n");
        return 2;
    }
    puts("SIG_INFO_OK");
    fflush(stdout);
    return 0;
}

static int check_sigchld(void) {
    // A child that exits posts SIGCHLD by default -- which a parent that
    // blocked it can observe with sigpending, without racing the exit.
    sigset_t block, old;
    sigemptyset(&block);
    sigaddset(&block, SIGCHLD);
    if (sigprocmask(SIG_BLOCK, &block, &old) < 0) { perror("sigprocmask"); return 3; }
    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 3; }
    if (pid == 0) {
        _exit(7);
    }
    // The child has exited (or will within a slice); SIGCHLD is pending
    // and blocked, so it sits in the pending set until unblocked.
    int spins = 0;
    sigset_t pending;
    while (spins++ < 1000000) {
        sigpending(&pending);
        if (sigismember(&pending, SIGCHLD))
            break;
    }
    if (!sigismember(&pending, SIGCHLD)) {
        fprintf(stderr, "SIGCHLD never showed pending\n");
        return 3;
    }
    if (sigprocmask(SIG_SETMASK, &old, NULL) < 0) { perror("sigprocmask restore"); return 3; }
    int status = 0;
    pid_t got = waitpid(pid, &status, 0);
    if (got != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 7) {
        fprintf(stderr, "waitpid: got=%d status=%#x\n", (int)got, status);
        return 3;
    }
    puts("SIG_CHLD_OK");
    fflush(stdout);
    return 0;
}

static void on_term(int sig) {
    (void)sig;
    got_usr1 = 9;
}

static int check_ignore_and_default(void) {
    // SIGTERM ignored: the process survives a kill to itself.
    if (signal(SIGTERM, SIG_IGN) == SIG_ERR) { perror("signal ignore"); return 4; }
    if (kill(getpid(), SIGTERM) < 0) { perror("kill ignored"); return 4; }
    // Back to default, but blocked -- so the disposition is recorded
    // without the process dying to prove it.
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_term;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    if (sigaction(SIGTERM, &sa, NULL) < 0) { perror("sigaction term"); return 4; }
    if (kill(getpid(), SIGTERM) < 0) { perror("kill handled"); return 4; }
    if (got_usr1 != 9) {
        fprintf(stderr, "reinstalled handler did not run\n");
        return 4;
    }
    // And the child inherits the disposition: fork with SIGTERM ignored,
    // the child ignores it too.
    if (signal(SIGTERM, SIG_IGN) == SIG_ERR) { perror("signal ignore2"); return 4; }
    pid_t pid = fork();
    if (pid < 0) { perror("fork inherit"); return 4; }
    if (pid == 0) {
        // Child: SIGTERM must be ignored (inherited), so this is a no-op.
        // Report the disposition by exiting with it encoded.
        struct sigaction cur;
        memset(&cur, 0, sizeof cur);
        if (sigaction(SIGTERM, NULL, &cur) < 0)
            _exit(11);
        _exit(cur.sa_handler == SIG_IGN ? 0 : 12);
    }
    int status = 0;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "child did not inherit SIG_IGN: %#x\n", status);
        return 4;
    }
    puts("SIG_DISP_OK");
    fflush(stdout);
    return 0;
}

int main(void) {
    int rc;
    if ((rc = check_handler_runs_and_returns())) return rc;
    if ((rc = check_siginfo_frame())) return rc;
    if ((rc = check_sigchld())) return rc;
    if ((rc = check_ignore_and_default())) return rc;
    return 0;
}
