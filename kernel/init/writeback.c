#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>

/* Shared writeback: a MAP_SHARED mapping is the same pages as the file,
 * so a write through the mapping must be readable through the file --
 * after munmap, and after msync while still mapped.
 *
 * One PIE binary in the shape of runtime.c/soak.c: a marker per check,
 * perror and a distinct exit code on failure.
 */
static int check_munmap_writeback(void) {
    char path[] = "/tmp/wbXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) { perror("mkstemp"); return 1; }
    if (ftruncate(fd, 8192) < 0) { perror("ftruncate"); return 1; }
    char *mp = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mp == MAP_FAILED) { perror("mmap shared"); return 1; }
    memcpy(mp, "written-shared", 15);
    memcpy(mp + 4096, "second-page!", 12);
    if (munmap(mp, 8192) < 0) { perror("munmap"); return 1; }
    // Read back through the file: the mapping is gone, the bytes stay.
    char buf[16] = { 0 };
    if (pread(fd, buf, 15, 0) != 15 || memcmp(buf, "written-shared", 15) != 0) {
        fprintf(stderr, "writeback: first page mismatch %15s\n", buf);
        return 1;
    }
    memset(buf, 0, sizeof buf);
    if (pread(fd, buf, 12, 4096) != 12 || memcmp(buf, "second-page!", 12) != 0) {
        fprintf(stderr, "writeback: second page mismatch\n");
        return 1;
    }
    close(fd);
    unlink(path);
    puts("WB_MUNMAP_OK");
    fflush(stdout);
    return 0;
}

static int check_msync_writeback(void) {
    char path[] = "/tmp/msXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) { perror("mkstemp msync"); return 2; }
    if (ftruncate(fd, 4096) < 0) { perror("ftruncate msync"); return 2; }
    char *mp = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mp == MAP_FAILED) { perror("mmap shared msync"); return 2; }
    memcpy(mp, "msync-bytes", 12);
    if (msync(mp, 4096, MS_SYNC) < 0) { perror("msync"); return 2; }
    // Still mapped, already visible through the file.
    char buf[16];
    memset(buf, 0, sizeof buf);
    ssize_t nr = pread(fd, buf, 12, 0);
    if (nr != 12 || memcmp(buf, "msync-bytes", 12) != 0) {
        fprintf(stderr, "msync: bytes not visible\n");
        return 2;
    }
    if (munmap(mp, 4096) < 0) { perror("munmap msync"); return 2; }
    close(fd);
    unlink(path);
    puts("WB_MSYNC_OK");
    fflush(stdout);
    return 0;
}

static int check_private_unaffected(void) {
    // A MAP_PRIVATE mapping of the same file must not see shared writes
    // made after it was taken, and must not write back its own.
    char path[] = "/tmp/pvXXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) { perror("mkstemp private"); return 3; }
    if (ftruncate(fd, 4096) < 0) { perror("ftruncate private"); return 3; }
    if (write(fd, "original", 9) != 9) { perror("write original"); return 3; }
    char *priv = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (priv == MAP_FAILED) { perror("mmap private"); return 3; }
    char *sh = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (sh == MAP_FAILED) { perror("mmap shared"); return 3; }
    memcpy(sh, "changed!!", 10);
    if (munmap(sh, 4096) < 0) { perror("munmap shared"); return 3; }
    // The private snapshot predates the shared write: still "original".
    if (memcmp(priv, "original", 8) != 0) {
        fprintf(stderr, "private snapshot polluted: %.8s\n", priv);
        return 3;
    }
    // And the private mapping's own writes never reach the file.
    memcpy(priv, "priv-only", 10);
    if (munmap(priv, 4096) < 0) { perror("munmap private"); return 3; }
    char buf[10] = { 0 };
    if (pread(fd, buf, 9, 0) != 9 || memcmp(buf, "changed!!", 9) != 0) {
        fprintf(stderr, "private write leaked or shared lost: %.9s\n", buf);
        return 3;
    }
    close(fd);
    unlink(path);
    puts("WB_PRIVATE_OK");
    fflush(stdout);
    return 0;
}

static int check_anon_shared(void) {
    // MAP_SHARED|MAP_ANONYMOUS: zeroed pages, writable, unmappable, and --
    // with no file -- nothing to write back. Survives as ordinary memory.
    char *mp = mmap(NULL, 8192, PROT_READ | PROT_WRITE,
                    MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (mp == MAP_FAILED) { perror("mmap anon shared"); return 4; }
    for (int i = 0; i < 8192; i++) {
        if (mp[i] != 0) {
            fprintf(stderr, "anon shared not zeroed at %d\n", i);
            return 4;
        }
    }
    memcpy(mp, "anon-data", 10);
    if (memcmp(mp, "anon-data", 9) != 0) {
        fprintf(stderr, "anon shared readback failed\n");
        return 4;
    }
    if (munmap(mp, 8192) < 0) { perror("munmap anon"); return 4; }
    puts("WB_ANON_OK");
    fflush(stdout);
    return 0;
}

int main(void) {
    int rc;
    if ((rc = check_munmap_writeback())) return rc;
    if ((rc = check_msync_writeback())) return rc;
    if ((rc = check_private_unaffected())) return rc;
    if ((rc = check_anon_shared())) return rc;
    return 0;
}
