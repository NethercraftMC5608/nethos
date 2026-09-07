/* Does WebKit load on nk?
 *
 * The same first question Mesa needed. WebKit is the largest thing we have
 * ever asked nk to map -- a closure of ~100MB across dozens of libraries --
 * and the first thing to settle is not whether it renders but whether the
 * loader can bring it in at all. A dlopen that fails at the twentieth
 * DT_NEEDED tells us the address space is short; one that succeeds turns
 * "will WebKit fit" from an argument into a measurement.
 *
 * Deliberately dlopen rather than link: the failure is then a message and a
 * library name instead of a loader error before main().
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>

/* Debian ships WebKitGTK under two sonames depending on the GTK it wants.
 * Try both; report which one answered. */
/* Loaded one at a time, smallest first, so the last one that prints is the
 * one whose constructor trapped. A single dlopen of WebKit pulls all 172 in
 * at once and tells you only that something in there died. */
static const char *CANDIDATES[] = {
    "libglib-2.0.so.0",
    "libgobject-2.0.so.0",
    "libcairo.so.2",
    "libgtk-4.so.1",
    "libjavascriptcoregtk-6.0.so.1",
    "libwebkitgtk-6.0.so.4",
    NULL,
};

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);

    void *h = NULL;
    for (int i = 0; CANDIDATES[i]; i++) {
        printf("  wk: opening %s\n", CANDIDATES[i]);
        void *o = dlopen(CANDIDATES[i], RTLD_NOW | RTLD_GLOBAL);
        if (!o) {
            printf("  wk: %s -> %s\n", CANDIDATES[i], dlerror());
            continue;
        }
        printf("  wk: ok %s\n", CANDIDATES[i]);
        h = o;                          /* the last one that opened */
    }
    if (!h) {
        printf("WK_DLOPEN_FAIL\n");
        return 1;
    }
    printf("WK_DLOPEN_OK\n");

    /* Calling in proves relocation finished and the text is executable, not
     * merely that the file was mapped. */
    unsigned (*major)(void) = dlsym(h, "webkit_get_major_version");
    unsigned (*minor)(void) = dlsym(h, "webkit_get_minor_version");
    unsigned (*micro)(void) = dlsym(h, "webkit_get_micro_version");
    if (!major || !minor || !micro) {
        printf("WK_DLSYM_FAIL\n");
        return 2;
    }
    printf("WK_VERSION %u.%u.%u\n", major(), minor(), micro());
    printf("WK_PROBE_OK\n");
    return 0;
}
