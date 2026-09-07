/* Mesa on nk: a GL context, and something rendered in it.
 *
 * llvmpipe rather than the GPU. virtio-gpu without virgl gives Linux a
 * display and dumb buffers, not a command stream a 3D driver could use, so
 * the honest first target is software rasterisation -- which is not a
 * consolation prize here: llvmpipe is the part of Mesa that leans hardest on
 * exactly what nk gained last (threads, dlopen, private file mappings, TLS
 * in dlopened libraries) and barely touches the GPU at all.
 *
 * Surfaceless, so there is no window system to stand up: EGL renders into a
 * framebuffer object and glReadPixels takes the result back. Reading the
 * pixels is the point -- a context that was created and never rendered
 * proves the loader worked, not the rasteriser.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <dlfcn.h>

#define W 64
#define H 64

int main(void)
{
    /* libEGL.so.1 is glvnd, a dispatch layer that dlopens the real Mesa
     * vendor and says nothing at all if that fails -- eglGetDisplay simply
     * returns EGL_NO_DISPLAY, which reads like a platform problem and is
     * usually a missing file. Load it here first so a failure names itself. */
    void *vendor = dlopen("libEGL_mesa.so.0", RTLD_NOW | RTLD_LOCAL);
    if (!vendor) {
        printf("EGL_VENDOR_DLOPEN_FAIL %s\n", dlerror());
        return 1;
    }
    printf("EGL_VENDOR_DLOPEN_OK\n");

    EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (dpy == EGL_NO_DISPLAY) {
        printf("EGL_DISPLAY_FAIL\n");
        return 1;
    }
    EGLint major = 0, minor = 0;
    if (!eglInitialize(dpy, &major, &minor)) {
        printf("EGL_INIT_FAIL %#x\n", eglGetError());
        return 1;
    }
    printf("EGL_INIT_OK %d.%d\n", major, minor);
    printf("EGL_VENDOR %s\n", eglQueryString(dpy, EGL_VENDOR));

    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        printf("EGL_BINDAPI_FAIL\n");
        return 1;
    }

    /* EGL_SURFACE_TYPE is asked for as 0 rather than left out: the default
     * is EGL_WINDOW_BIT, and a surfaceless display has no config that can
     * make a window, so the default matches nothing and eglChooseConfig
     * succeeds while returning none. Colour depths are left out for the same
     * reason -- the context never presents, so the only thing the config has
     * to agree about is the client API. */
    const EGLint cfg_attrs[] = {
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_SURFACE_TYPE, 0,
        EGL_NONE
    };
    EGLConfig cfg;
    EGLint n = 0;
    if (!eglChooseConfig(dpy, cfg_attrs, &cfg, 1, &n) || n < 1) {
        printf("EGL_CONFIG_FAIL\n");
        return 1;
    }

    const EGLint ctx_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctx_attrs);
    if (ctx == EGL_NO_CONTEXT) {
        printf("EGL_CONTEXT_FAIL %#x\n", eglGetError());
        return 1;
    }
    if (!eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) {
        printf("EGL_MAKECURRENT_FAIL %#x\n", eglGetError());
        return 1;
    }
    printf("GL_VENDOR %s\n", glGetString(GL_VENDOR));
    printf("GL_RENDERER %s\n", glGetString(GL_RENDERER));
    printf("GL_VERSION %s\n", glGetString(GL_VERSION));

    /* An FBO, because surfaceless has no default framebuffer to draw to. */
    GLuint tex, fbo;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                           GL_TEXTURE_2D, tex, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        printf("GL_FBO_FAIL\n");
        return 1;
    }

    /* A colour no uninitialised buffer would plausibly hold, so reading it
     * back cannot pass by accident. */
    glViewport(0, 0, W, H);
    glClearColor(0.25f, 0.5f, 0.75f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();

    unsigned char px[4] = {0, 0, 0, 0};
    glReadPixels(W / 2, H / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
    printf("GL_PIXEL %d %d %d %d\n", px[0], px[1], px[2], px[3]);

    /* 0.25 * 255 = 63.75, and rounding is the driver's business, so allow
     * either side rather than asserting a rasteriser's rounding mode. */
    if (abs(px[0] - 64) > 2 || abs(px[1] - 128) > 2 || abs(px[2] - 191) > 2) {
        printf("GL_PIXEL_WRONG\n");
        return 1;
    }
    printf("MESA_OK\n");
    return 0;
}
