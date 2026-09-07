/* M2 compositor proof: hand-rolled Wayland display + client, std C only.
 *
 * No libwayland: the wire protocol is small enough to speak directly
 * (wayland.xml from libwayland-dev in nethos-ldk, version 1.23.1).
 * The server binds a unix socket at $XDG_RUNTIME_DIR/wayland-0,
 * the client connects, sends wl_display@1.get_registry, and prints
 * each wl_registry.global triple it is offered as a WL_GLOBAL line,
 * then WL_REGISTRY_OK + WL_CLIENT_OK. The globals offered include
 * wl_compositor + wl_shm (the two M3 needs) plus wl_output + xdg_wm_base.
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
 *  wl_display events: 0=error(o, u, s), 1=delete_id(u).
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#define DIE(...) do { fprintf(stderr, "WL_FAIL " __VA_ARGS__); fprintf(stderr, "\n"); _exit(1); } while (0)

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

typedef struct { const char *iface; uint32_t version; } global_t;

static const global_t GLOBALS[] = {
    { "wl_compositor", 4 },
    { "wl_shm", 1 },
    { "wl_output", 2 },
    { "xdg_wm_base", 2 },
};
#define NGLOBALS (sizeof GLOBALS / sizeof GLOBALS[0])

/* ---- server: one client, blocking send/recv, then exit -------------- */
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
    close(c);
    close(ls);
    printf("WL_SERVER_DONE\n");
    fflush(stdout);
    _exit(0);
}

/* ---- client: connect, get_registry, print each global ----------------- */
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
    printf("WL_CLIENT_CONNECTED\n");
    fflush(stdout);

    uint32_t reg_id = 2; /* first client id after wl_display@1 */
    msg_t m;
    msg_begin(&m, 1, 1); /* wl_display.get_registry */
    msg_u(&m, reg_id);
    if (msg_send(s, &m) < 0) { perror("WL_FAIL send get_registry"); exit(1); }

    /* Read until EOF: each global event prints one marker line. */
    uint8_t buf[8192];
    uint32_t len = 0;
    int nglobal = 0;
    for (;;) {
        ssize_t r = recv(s, buf + len, sizeof buf - len, 0);
        if (r < 0) { perror("WL_FAIL recv"); exit(1); }
        if (r == 0) break;
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
    if (nglobal != NGLOBALS) {
        printf("WL_FAIL globals=%d want=%u\n", nglobal, (unsigned)NGLOBALS);
        fflush(stdout);
        exit(1);
    }
    printf("WL_REGISTRY_OK globals=%d\n", nglobal);
    fflush(stdout);
    int saw_comp = 0, saw_shm = 0;
    (void)saw_comp; (void)saw_shm;
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
