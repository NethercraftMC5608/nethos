/* M2 compositor proof + M3 buffer-path proof: hand-rolled Wayland display +
 * client, std C only.
 *
 * No libwayland: the wire protocol is small enough to speak directly
 * (wayland.xml from libwayland-dev in nethos-ldk, version 1.23.1).
 * The server binds a unix socket at $XDG_RUNTIME_DIR/wayland-0.
 *
 * Phase 1 (M2, green): the client connects, sends wl_display@1.get_registry,
 *   and prints each wl_registry.global triple it is offered as a WL_GLOBAL
 *   line, then WL_REGISTRY_OK + WL_CLIENT_OK. The globals offered include
 *   wl_compositor + wl_shm (the two M3 needs) plus wl_output + xdg_wm_base.
 * Phase 2 (M3 step A): the client binds wl_compositor + wl_shm out of the
 *   registry it already has (wl_registry.bind), creates a memfd, fills it
 *   with the shell's dark-slate pixel (0xFF14181F -- the first stop of the
 *   slate wallpaper gradient, payload/shell/style.css:1229), passes the fd
 *   with wl_shm.create_pool over SCM_RIGHTS, and prints SHM_CLIENT_CKSUM
 *   (FNV-1a over the pool). The server mmaps the received fd MAP_SHARED,
 *   verifies head/tail magic + interior pixels, and prints SHM_SERVER_CKSUM
 *   + SHM_OK. Equal checksums in the serial log prove the exact M3 pixel
 *   path (compositor reads client pixels through a shared mapping) minus
 *   the display. No display, no DRM mmap, no WebKit in this step.
 *
 * Wire format truth (wayland.xml + protocol.c in libwayland source):
 *  message = [sender id u32][opcode u16][size u16][payload...]
 *  payload arg types: u=new_id/i=int/u=uint/f=fixed/h=fd untouched,
 *   s=string: u32 len incl. NUL, bytes+NUL, padded to 4; o=object: u32 id.
 *  wl_display object is always id 1 (libwayland reserves id 1 for display).
 *  Client ids grow 2,3,4...; server-created ids must be >= 0xff000000
 *   (libwayland id-allocator convention, server namespace).
 *  wl_display requests: 0=sync(new_id wl_callback), 1=get_registry(new_id).
 *  wl_registry events: 0=global(u name, s interface, u version),
 *   1=global_remove(u name).
 *  wl_registry requests: 0=bind(u name, s interface, u version, new_id id).
 *  wl_shm requests: 0=create_pool(new_id wl_shm_pool, h fd, i size).
 *  wl_shm.format enum: 0=argb8888, 1=xrgb8888 (this probe uses xrgb8888:
 *   every pixel opaque, no alpha interpretation anywhere).
 *  wl_display events: 0=error(o, u, s), 1=delete_id(u).
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#define DIE(...) do { fprintf(stderr, "WL_FAIL " __VA_ARGS__); fprintf(stderr, "\n"); _exit(1); } while (0)

/* Client-allocated ids: registry 2, compositor 3, shm 4, pool 5. */
#define ID_REGISTRY 2u
#define ID_COMPOSITOR 3u
#define ID_SHM 4u
#define ID_POOL 5u

/* The M3 pool: 64x64 xrgb8888, one page-ish of slate pixels.
 * A second flavour byte sits in the middle of the pool (offset SHM_SIZE/2):
 * it separates "shared pages" (whole pool readable) from "coherent bytes"
 * (a client write after the server mapped is visible without re-map). */
#define SHM_W 64
#define SHM_H 64
#define SHM_STRIDE (SHM_W * 4)
#define SHM_SIZE (SHM_STRIDE * SHM_H)
#define SHM_BASE_PX 0xFF14181Fu /* slate #14181f, opaque */
#define SHM_HEAD_MAGIC 0x5753484Du
#define SHM_TAIL_MAGIC 0x4D485357u
#define SHM_COH_OFF (SHM_SIZE / 2)
#define SHM_COH_PX 0xFF2A7F62u /* signal violet: only the post-map write */

static uint32_t fnv1a(const uint8_t *p, uint32_t n)
{
    uint32_t h = 0x811c9dc5u;
    for (uint32_t i = 0; i < n; i++) {
        h ^= p[i];
        h *= 0x01000193u;
    }
    return h;
}

/* Fill the pool pattern both sides share: magic head/tail so a stray or
 * zero page can never verify, slate pixel everywhere else, and the flavour
 * word at SHM_COH_OFF preset to slate (the client overwrites it after the
 * server has mapped, to test post-map coherence). */
static void shm_fill(uint32_t *px)
{
    uint32_t n = SHM_SIZE / 4;
    for (uint32_t i = 0; i < n; i++)
        px[i] = SHM_BASE_PX;
    px[0] = SHM_HEAD_MAGIC;
    px[n - 1] = SHM_TAIL_MAGIC;
    px[SHM_COH_OFF / 4] = SHM_BASE_PX;
}

/* Returns 0 when every word is exactly the pattern shm_fill writes.
 * Mode 1 additionally requires the coherence word to be SHM_COH_PX
 * (the client's post-map write landed in the server's mapping). */
static int shm_verify(const uint32_t *px, int coh)
{
    uint32_t n = SHM_SIZE / 4;
    if (px[0] != SHM_HEAD_MAGIC)
        return 1;
    if (px[n - 1] != SHM_TAIL_MAGIC)
        return 2;
    for (uint32_t i = 1; i < n - 1; i++) {
        if (i == SHM_COH_OFF / 4)
            continue;
        if (px[i] != SHM_BASE_PX)
            return 3;
    }
    uint32_t want = coh ? SHM_COH_PX : SHM_BASE_PX;
    if (px[SHM_COH_OFF / 4] != want)
        return 4;
    return 0;
}

/* ---- message builder: sender, opcode, then args, then fix the u16 size --- */
typedef struct { uint8_t b[4096]; uint32_t n; } msg_t;

static void put32(msg_t *m, uint32_t v) { memcpy(m->b + m->n, &v, 4); m->n += 4; }
static void put16(msg_t *m, uint16_t v) { memcpy(m->b + m->n, &v, 2); m->n += 2; }

static void msg_begin(msg_t *m, uint32_t sender, uint16_t opcode) {
    m->n = 0;
    put32(m, sender);
    put16(m, opcode);
    put16(m, 0); /* size filled in by msg_send */
}

static void msg_u(msg_t *m, uint32_t v) { put32(m, v); }

static void msg_s(msg_t *m, const char *s) {
    uint32_t len = (uint32_t)strlen(s) + 1;
    put32(m, len);
    memcpy(m->b + m->n, s, len);
    m->n += len;
    while (m->n & 3) m->b[m->n++] = 0;
}

static int msg_send(int fd, msg_t *m) {
    uint16_t sz = (uint16_t)m->n;
    memcpy(m->b + 6, &sz, 2);
    uint32_t off = 0;
    while (off < m->n) {
        ssize_t w = send(fd, m->b + off, m->n - off, 0);
        if (w <= 0) return -1;
        off += (uint32_t)w;
    }
    return 0;
}

/* One message + one fd over SCM_RIGHTS (wl_shm.create_pool). */
static int msg_send_fd(int sock, msg_t *m, int fd) {
    uint16_t sz = (uint16_t)m->n;
    memcpy(m->b + 6, &sz, 2);
    struct iovec io;
    io.iov_base = m->b;
    io.iov_len = m->n;
    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr mh;
    memset(&mh, 0, sizeof mh);
    mh.msg_iov = &io;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof cbuf;
    struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &fd, sizeof fd);
    ssize_t w = sendmsg(sock, &mh, 0);
    return (w == (ssize_t)m->n) ? 0 : -1;
}

/* Best-effort receive deadline so a wedge prints WL_FAIL instead of hanging
 * until the outer --timeout kills the machine. Failure to set it is not
 * fatal: the watchdog still bounds the boot. */
static void recv_deadline(int s) {
    struct timeval tv;
    tv.tv_sec = 20;
    tv.tv_usec = 0;
    setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
}

typedef struct { const char *iface; uint32_t version; } global_t;

static const global_t GLOBALS[] = {
    { "wl_compositor", 4 },
    { "wl_shm", 1 },
    { "wl_output", 2 },
    { "xdg_wm_base", 2 },
};
#define NGLOBALS (sizeof GLOBALS / sizeof GLOBALS[0])

/* ---- server: one client, registry first, then the shm pool phase -------- */
static void server(const char *path) {
    unlink(path);
    int ls = socket(AF_UNIX, SOCK_STREAM, 0);
    if (ls < 0) { perror("WL_FAIL socket"); _exit(1); }
    struct sockaddr_un a;
    memset(&a, 0, sizeof a);
    a.sun_family = AF_UNIX;
    snprintf(a.sun_path, sizeof a.sun_path, "%s", path);
    if (bind(ls, (struct sockaddr *)&a, sizeof a) < 0) { perror("WL_FAIL bind"); _exit(1); }
    if (listen(ls, 1) < 0) { perror("WL_FAIL listen"); _exit(1); }
    printf("WL_SERVER_LISTEN %s\n", path);
    fflush(stdout);
    int c = accept(ls, NULL, NULL);
    if (c < 0) { perror("WL_FAIL accept"); _exit(1); }
    recv_deadline(c);
    printf("WL_SERVER_ACCEPT\n");
    fflush(stdout);

    /* One request: expect [1][get_registry=1][8][new_id]. */
    uint8_t req[64];
    uint32_t got = 0;
    while (got < 12) {
        ssize_t r = recv(c, req + got, 12 - got, 0);
        if (r <= 0) DIE("server recv: %s", r ? strerror(errno) : "EOF");
        got += (uint32_t)r;
    }
    uint32_t sender;
    uint16_t opcode, size;
    memcpy(&sender, req, 4);
    memcpy(&opcode, req + 4, 2);
    memcpy(&size, req + 6, 2);
    uint32_t reg_id;
    memcpy(&reg_id, req + 8, 4);
    if (sender != 1 || opcode != 1 || size != 12) DIE("server bad get_registry sender=%u op=%u size=%u", sender, opcode, size);
    printf("WL_SERVER_GET_REGISTRY id=%u\n", reg_id);
    fflush(stdout);

    for (uint32_t i = 0; i < NGLOBALS; i++) {
        msg_t m;
        msg_begin(&m, reg_id, 0); /* wl_registry.global */
        msg_u(&m, i + 1);
        msg_s(&m, GLOBALS[i].iface);
        msg_u(&m, GLOBALS[i].version);
        if (msg_send(c, &m) < 0) DIE("server send global %u: %s", i + 1, strerror(errno));
        printf("WL_SERVER_SENT %u %s %u\n", i + 1, GLOBALS[i].iface, GLOBALS[i].version);
        fflush(stdout);
    }

    /* Phase 2: expect wl_registry.bind x2 (compositor, shm) then
     * wl_shm.create_pool carrying the pool fd. Stream-coalesced or split,
     * so accumulate bytes and parse whole messages; ancillary data lands
     * on whichever recvmsg carries the pool bytes. */
    uint8_t sbuf[4096];
    uint32_t slen = 0;
    int got_comp = 0, got_shm = 0, got_pool = 0;
    uint32_t pool_id = 0;
    int32_t pool_size = 0;
    int pool_fd = -1;
    while (!(got_comp && got_shm && got_pool)) {
        uint8_t tmp[1024];
        char cbuf[256];
        struct iovec io;
        io.iov_base = tmp;
        io.iov_len = sizeof tmp;
        struct msghdr mh;
        memset(&mh, 0, sizeof mh);
        mh.msg_iov = &io;
        mh.msg_iovlen = 1;
        mh.msg_control = cbuf;
        mh.msg_controllen = sizeof cbuf;
        ssize_t r = recvmsg(c, &mh, 0);
        if (r < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                DIE("server shm phase: recv timeout");
            DIE("server shm phase recv: %s", strerror(errno));
        }
        if (r == 0) DIE("server EOF in shm phase (binds=%d/%d pool=%d)", got_comp, got_shm, got_pool);
        struct cmsghdr *cm;
        for (cm = CMSG_FIRSTHDR(&mh); cm; cm = CMSG_NXTHDR(&mh, cm)) {
            if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS &&
                cm->cmsg_len >= CMSG_LEN(sizeof(int)))
                memcpy(&pool_fd, CMSG_DATA(cm), sizeof pool_fd);
        }
        if (slen + (uint32_t)r > sizeof sbuf) DIE("server shm phase overflow");
        memcpy(sbuf + slen, tmp, (uint32_t)r);
        slen += (uint32_t)r;
        uint32_t off = 0;
        while (slen - off >= 8) {
            uint32_t sdr;
            uint16_t opc, sz;
            memcpy(&sdr, sbuf + off, 4);
            memcpy(&opc, sbuf + off + 4, 2);
            memcpy(&sz, sbuf + off + 6, 2);
            if (sz < 8 || (sz & 3)) DIE("server bad size %u", sz);
            if (slen - off < sz) break; /* partial: wait for more */
            if (sdr == reg_id && opc == 0) {
                /* wl_registry.bind: u name, s interface, u version, new_id */
                uint32_t name, ilen, version, new_id;
                memcpy(&name, sbuf + off + 8, 4);
                memcpy(&ilen, sbuf + off + 12, 4);
                if (ilen == 0 || ilen > sz - 16) DIE("server bad bind string %u in %u", ilen, sz);
                char iface[128];
                uint32_t cp = ilen < sizeof iface ? ilen : sizeof iface;
                memcpy(iface, sbuf + off + 16, cp);
                iface[sizeof iface - 1] = 0;
                uint32_t voff = off + 16 + ((ilen + 3) & ~3u);
                memcpy(&version, sbuf + voff, 4);
                memcpy(&new_id, sbuf + voff + 4, 4);
                printf("WL_SERVER_BIND name=%u interface=%s version=%u id=%u\n",
                       name, iface, version, new_id);
                fflush(stdout);
                if (name == 1 && !strcmp(iface, "wl_compositor") && new_id == ID_COMPOSITOR)
                    got_comp = 1;
                else if (name == 2 && !strcmp(iface, "wl_shm") && new_id == ID_SHM)
                    got_shm = 1;
                else
                    DIE("server unexpected bind name=%u iface=%s id=%u", name, iface, new_id);
            } else if (sdr == ID_SHM && opc == 0) {
                /* wl_shm.create_pool: new_id pool, h fd (ancillary), i size */
                if (sz != 16) DIE("server bad create_pool size %u", sz);
                memcpy(&pool_id, sbuf + off + 8, 4);
                memcpy(&pool_size, sbuf + off + 12, 4);
                printf("WL_SERVER_POOL id=%u size=%d fd=%d\n", pool_id, pool_size, pool_fd);
                fflush(stdout);
                if (pool_id != ID_POOL || pool_size != SHM_SIZE)
                    DIE("server bad pool id=%u size=%d", pool_id, pool_size);
                got_pool = 1;
            } else {
                DIE("server unexpected msg sender=%u opcode=%u size=%u", sdr, opc, sz);
            }
            off += sz;
        }
        if (off > 0) { memmove(sbuf, sbuf + off, slen - off); slen -= off; }
    }
    if (pool_fd < 0) DIE("server got pool message but no fd");
    printf("SHM_POOL_FD_OK\n");
    fflush(stdout);
    /* PROT_READ|PROT_WRITE on purpose: a PROT_READ-only shared mapping is
     * currently unusable on nk -- map_shared + protect_user_none clears
     * AP bit 6, turning AP_RO_ANY (0xC0) into AP=2 (EL0 no-access), so the
     * first EL0 read faults with DFSC 0x0f (measured /tmp/m3-stepA-1.log:
     * esr 0x9200000f far 0x2ee2b000 pc 0x4023ac, L3 0x0060000045566f87).
     * That is a kernel-lane bug to fix, not this lane's: the compositor
     * only reads here, and writable-but-unwritten still proves the same
     * shared pages. See M3 verdict. */
    void *map = mmap(NULL, SHM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, pool_fd, 0);
    if (map == MAP_FAILED) DIE("server mmap pool: %s", strerror(errno));
    /* Snapshot 1: the pool as handed over. Verifies the shared pages
     * (head/tail magic + slate interior), prints the checksum the proof
     * compares against the client's. */
    uint32_t cksum = fnv1a((const uint8_t *)map, SHM_SIZE);
    printf("SHM_SERVER_CKSUM 0x%08x\n", cksum);
    fflush(stdout);
    int bad = shm_verify((const uint32_t *)map, 0);
    if (bad) DIE("server pool bytes wrong (class %d)", bad);
    printf("SHM_SNAP1_OK\n");
    fflush(stdout);
    /* Snapshot 2 (coherence): tell the client its post-map write may land,
     * then poll the flavour word for it. nk's shared mappings are eager
     * same-physical-pages with no coherence against later writes through
     * other descriptors (shm.rs), so this may or may not arrive -- the
     * marker records which, either way, with a number. */
    uint8_t one = 'G';
    if (send(c, &one, 1, 0) != 1) DIE("server send go: %s", strerror(errno));
    printf("SHM_COH_WAIT\n");
    fflush(stdout);
    volatile uint32_t *flav = (volatile uint32_t *)((uint8_t *)map + SHM_COH_OFF);
    int coh = 0;
    for (int i = 0; i < 200; i++) {
        if (*flav == SHM_COH_PX) { coh = 1; break; }
        usleep(50000);
    }
    printf("SHM_COH_READ 0x%08x\n", *flav);
    fflush(stdout);
    bad = shm_verify((const uint32_t *)map, coh);
    munmap(map, SHM_SIZE);
    close(pool_fd);
    if (bad) DIE("server pool bytes wrong (class %d)", bad);
    printf("SHM_OK size=%d format=xrgb8888 stride=%d coh=%d\n", SHM_SIZE, SHM_STRIDE, coh);
    fflush(stdout);
    close(c);
    close(ls);
    printf("WL_SERVER_DONE\n");
    fflush(stdout);
    _exit(0);
}

/* ---- client: registry, then bind + pool --------------------------------- */
static void client(const char *path) {
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    if (s < 0) { perror("WL_FAIL client socket"); exit(1); }
    struct sockaddr_un a;
    memset(&a, 0, sizeof a);
    a.sun_family = AF_UNIX;
    snprintf(a.sun_path, sizeof a.sun_path, "%s", path);
    /* Server forks just before us; retry briefly rather than racing it. */
    int ok = -1;
    for (int i = 0; i < 100 && ok < 0; i++) {
        ok = connect(s, (struct sockaddr *)&a, sizeof a);
        if (ok < 0) usleep(50000);
    }
    if (ok < 0) { perror("WL_FAIL connect"); exit(1); }
    recv_deadline(s);
    printf("WL_CLIENT_CONNECTED\n");
    fflush(stdout);

    uint32_t reg_id = ID_REGISTRY; /* first client id after wl_display@1 */
    msg_t m;
    msg_begin(&m, 1, 1); /* wl_display.get_registry */
    msg_u(&m, reg_id);
    if (msg_send(s, &m) < 0) { perror("WL_FAIL send get_registry"); exit(1); }

    /* Read until all four globals are in (server keeps the socket open for
     * phase 2, so EOF is not the terminator here). */
    uint8_t buf[8192];
    uint32_t len = 0;
    int nglobal = 0;
    char ifaces[8][128];
    while (nglobal != (int)NGLOBALS) {
        ssize_t r = recv(s, buf + len, sizeof buf - len, 0);
        if (r < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                printf("WL_FAIL registry timeout globals=%d\n", nglobal);
                fflush(stdout);
                exit(1);
            }
            perror("WL_FAIL recv");
            exit(1);
        }
        if (r == 0) {
            printf("WL_FAIL early EOF globals=%d\n", nglobal);
            fflush(stdout);
            exit(1);
        }
        len += (uint32_t)r;
        /* Parse whole messages: header [sender u32][opcode u16][size u16]. */
        uint32_t off = 0;
        while (len - off >= 8) {
            uint32_t sender;
            uint16_t opcode, size;
            memcpy(&sender, buf + off, 4);
            memcpy(&opcode, buf + off + 4, 2);
            memcpy(&size, buf + off + 6, 2);
            if (size < 8 || (size & 3)) {
                printf("WL_FAIL bad size %u\n", size);
                fflush(stdout);
                exit(1);
            }
            if (len - off < size) break; /* partial: wait for more */
            if (sender == reg_id && opcode == 0) {
                /* global: u32 name, string iface, u32 version */
                uint32_t name, ilen, version;
                memcpy(&name, buf + off + 8, 4);
                memcpy(&ilen, buf + off + 12, 4);
                if (ilen == 0 || ilen > size - 16) {
                    printf("WL_FAIL bad string len %u in size %u\n", ilen, size);
                    fflush(stdout);
                    exit(1);
                }
                char iface[128];
                uint32_t cp = ilen < sizeof iface ? ilen : sizeof iface;
                memcpy(iface, buf + off + 16, cp);
                iface[sizeof iface - 1] = 0;
                uint32_t voff = off + 16 + ((ilen + 3) & ~3u);
                memcpy(&version, buf + voff, 4);
                printf("WL_GLOBAL name=%u interface=%s version=%u\n", name, iface, version);
                fflush(stdout);
                if (nglobal < 8) {
                    snprintf(ifaces[nglobal], sizeof ifaces[0], "%s", iface);
                }
                nglobal++;
            } else if (sender == reg_id && opcode == 1) {
                uint32_t name;
                memcpy(&name, buf + off + 8, 4);
                printf("WL_GLOBAL_REMOVE name=%u\n", name);
                fflush(stdout);
            } else {
                printf("WL_OTHER sender=%u opcode=%u size=%u\n", sender, opcode, size);
                fflush(stdout);
            }
            off += size;
        }
        if (off > 0) { memmove(buf, buf + off, len - off); len -= off; }
        if (len == sizeof buf) { printf("WL_FAIL overflow\n"); fflush(stdout); exit(1); }
    }
    int saw_comp = 0, saw_shm = 0;
    for (int i = 0; i < nglobal && i < 8; i++) {
        if (!strcmp(ifaces[i], "wl_compositor")) saw_comp = 1;
        if (!strcmp(ifaces[i], "wl_shm")) saw_shm = 1;
    }
    if (!saw_comp || !saw_shm) {
        printf("WL_FAIL missing globals comp=%d shm=%d\n", saw_comp, saw_shm);
        fflush(stdout);
        exit(1);
    }
    printf("WL_REGISTRY_OK globals=%d\n", nglobal);
    fflush(stdout);

    /* Phase 2: bind both globals, then hand over the pool. */
    msg_begin(&m, reg_id, 0); /* wl_registry.bind wl_compositor */
    msg_u(&m, 1);
    msg_s(&m, "wl_compositor");
    msg_u(&m, 4);
    msg_u(&m, ID_COMPOSITOR);
    if (msg_send(s, &m) < 0) { perror("WL_FAIL send bind comp"); exit(1); }
    msg_begin(&m, reg_id, 0); /* wl_registry.bind wl_shm */
    msg_u(&m, 2);
    msg_s(&m, "wl_shm");
    msg_u(&m, 1);
    msg_u(&m, ID_SHM);
    if (msg_send(s, &m) < 0) { perror("WL_FAIL send bind shm"); exit(1); }
    printf("WL_CLIENT_BOUND comp=%u shm=%u\n", ID_COMPOSITOR, ID_SHM);
    fflush(stdout);

    int fd = memfd_create("wl-shm-pool", MFD_CLOEXEC);
    if (fd < 0) { perror("WL_FAIL memfd_create"); exit(1); }
    if (ftruncate(fd, SHM_SIZE) < 0) { perror("WL_FAIL ftruncate"); exit(1); }
    void *map = mmap(NULL, SHM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) { perror("WL_FAIL mmap pool"); exit(1); }
    shm_fill((uint32_t *)map);
    uint32_t cksum = fnv1a((const uint8_t *)map, SHM_SIZE);
    printf("SHM_CLIENT_CKSUM 0x%08x\n", cksum);
    fflush(stdout);

    msg_begin(&m, ID_SHM, 0); /* wl_shm.create_pool */
    msg_u(&m, ID_POOL);
    /* fd travels ancillary (h args take no payload bytes) */
    put32(&m, SHM_SIZE); /* i size */
    if (msg_send_fd(s, &m, fd) < 0) { perror("WL_FAIL send create_pool"); exit(1); }
    printf("WL_CLIENT_POOL id=%u size=%d\n", ID_POOL, SHM_SIZE);
    fflush(stdout);

    /* Server closes after verifying; drain to EOF so a dead server cannot
     * read as success (main checks its exit status too). The server first
     * sends one 'G' byte once its mapping is up: that is the cue for the
     * post-map coherence write (SHM_COH_PX at SHM_COH_OFF), which must go
     * through the client's own mapping -- never pwrite, which would hide
     * behind writeback rather than proving shared pages. */
    int coh_sent = 0;
    for (;;) {
        uint8_t tail[256];
        ssize_t r = recv(s, tail, sizeof tail, 0);
        if (r < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                printf("WL_FAIL pool-ack timeout\n");
                fflush(stdout);
                exit(1);
            }
            perror("WL_FAIL recv tail");
            exit(1);
        }
        if (r == 0) break;
        if (!coh_sent) {
            ((volatile uint32_t *)map)[SHM_COH_OFF / 4] = SHM_COH_PX;
            uint32_t cksum2 = fnv1a((const uint8_t *)map, SHM_SIZE);
            printf("SHM_CLIENT_CKSUM2 0x%08x\n", cksum2);
            fflush(stdout);
            coh_sent = 1;
        }
    }
    munmap(map, SHM_SIZE);
    close(fd);
    printf("WL_CLIENT_OK\n");
    fflush(stdout);
    close(s);
}

/* ---- main: fork server, run client in parent -------------------------- */
int main(void) {
    const char *rtd = getenv("XDG_RUNTIME_DIR");
    if (!rtd || !rtd[0]) rtd = "/tmp";
    char path[108];
    snprintf(path, sizeof path, "%s/wayland-0", rtd);
    setvbuf(stdout, NULL, _IOLBF, 0);
    pid_t p = fork();
    if (p < 0) { perror("WL_FAIL fork"); return 1; }
    if (p == 0) server(path);
    client(path);
    int st = 0;
    pid_t w;
    int spins = 0;
    do {
        w = waitpid(p, &st, WNOHANG);
        if (w == 0) { usleep(50000); spins++; }
    } while (w == 0 && spins < 200);
    if (w == 0) { printf("WL_FAIL server still running\n"); fflush(stdout); return 1; }
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        printf("WL_FAIL server status=%d\n", st);
        fflush(stdout);
        return 1;
    }
    printf("WL_PROBE_OK\n");
    fflush(stdout);
    return 0;
}
