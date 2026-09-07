/* Does the Mesa loading mechanism work on nk?
 *
 * Mesa is not one library: libEGL dlopens libEGL_mesa, which dlopens a
 * _dri.so, which dlopens libgallium. Every one of those is a runtime open,
 * mmap, relocate and TLS allocation, and if any step is missing on nk then
 * the size of Mesa is irrelevant because none of it would load anyway.
 *
 * So this probes the mechanism rather than the renderer: dlopen a library
 * that was never on the link line, call through it, and use it to talk to
 * the DRM device nk's virtio_gpu already provides. libdrm is 132KB, which
 * fits in a memory-backed rootfs; libLLVM is 118MB, which does not. Proving
 * the mechanism first means the memory work that follows is known to be the
 * only thing left rather than the first of several unknowns.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/mount.h>

/* Declared here rather than included: the point is that nothing about this
 * program is linked against libdrm. */
typedef struct { int major, minor, patchlevel; int name_len; char *name; } drm_version_t;

int main(void)
{
    /* The other fixtures are shell scripts that mount this first; this one is
     * the init itself, so it does its own. Without devtmpfs /dev is an empty
     * directory and the GPU that demonstrably exists has no node to open. */
    if (mount("devtmpfs", "/dev", "devtmpfs", 0, NULL) != 0) {
        printf("DEVTMPFS_FAIL\n");
        return 1;
    }
    printf("DEVTMPFS_OK\n");

    void *h = dlopen("libdrm.so.2", RTLD_NOW);
    if (!h) {
        printf("DLOPEN_FAIL %s\n", dlerror());
        return 1;
    }
    printf("DLOPEN_OK\n");

    /* A thread-local in a dlopened library is the case that needs dynamic
     * TLS -- __tls_get_addr and a module id allocated at load time, not the
     * static block the initial-exec model uses. Mesa relies on it heavily. */
    void *(*get_version)(int) = dlsym(h, "drmGetVersion");
    void (*free_version)(void *) = dlsym(h, "drmFreeVersion");
    if (!get_version || !free_version) {
        printf("DLSYM_FAIL\n");
        return 1;
    }
    printf("DLSYM_OK\n");

    int fd = open("/dev/dri/card0", O_RDWR);
    if (fd < 0) {
        printf("DRM_OPEN_FAIL\n");
        return 1;
    }

    drm_version_t *v = get_version(fd);
    if (!v || !v->name) {
        printf("DRM_VERSION_FAIL\n");
        return 1;
    }
    printf("DRM_DRIVER %s %d.%d.%d\n", v->name, v->major, v->minor, v->patchlevel);
    free_version(v);
    close(fd);
    printf("DRMPROBE_OK\n");
    return 0;
}
