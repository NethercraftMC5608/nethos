"""Can nethosd run on nk?

nethosd is the desktop's API: stdlib Python, a ThreadingHTTPServer on
127.0.0.1:7777, and nothing else. So the question is not whether Python runs
-- npkg settled that -- but whether nk gives Linux a loopback interface and an
AF_INET stack to bind to.

Deliberately the same shape as nethosd rather than a socket unit test: a
threaded HTTP server, a real request over TCP, a real JSON response. A
loopback that carries one datagram and not a connection is a failure this
would catch and a `socket()` that returns a descriptor would not.
"""

import errno
import json
import os
import select
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST, PORT = "127.0.0.1", 7777


def step(name, ok, detail=""):
    print(f"{'NET_OK  ' if ok else 'NET_FAIL'} {name}{' ' + detail if detail else ''}",
          flush=True)
    return ok


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"path": self.path, "kernel": "nk"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass            # the console is the kernel log; do not narrate


def main():
    # 0. Does one blocked thread stall the whole kernel?
    #
    # The threaded HTTP server hangs while the identical single-threaded
    # sequence completes, so the variable is threads, not TCP. LKL is one
    # kernel behind a CPU lock, and a task that blocks inside it must give
    # that lock up; if it does not, the first thread to wait on anything
    # stops every other thread from making a syscall at all.
    #
    # This is the whole desktop's problem, not this probe's: nethosd is a
    # ThreadingHTTPServer, a compositor waits in one thread while rendering
    # in another, and WebKit is several processes each with a pool.
    r, w = os.pipe()

    def blocker():
        os.read(r, 1)               # never written to: blocks in Linux forever

    threading.Thread(target=blocker, daemon=True).start()
    time.sleep(0.5)                   # let it get inside the read
    print("  probe: a thread is now blocked in read()", flush=True)

    # Any syscall with a bounded wait will do. If Linux is still answering,
    # this returns empty after two seconds; if one blocked thread has taken
    # the kernel with it, nothing below this line ever prints.
    r2, w2 = os.pipe()
    poller = select.poll()
    poller.register(r2, select.POLLIN)
    t0 = time.monotonic()
    res = poller.poll(2000)
    waited = time.monotonic() - t0
    step("kernel-alive-with-blocked-thread", not res and waited >= 1.0,
         f"poll returned {res!r} after {waited:.2f}s")
    for fd in (r, w, r2, w2):
        try:
            os.close(fd)
        except OSError:
            pass

    # 0b. Do threads share one file-descriptor table?
    #
    # CLONE_FILES is part of what pthread_create asks for: threads share one
    # table, so a descriptor opened by any of them is immediately visible to
    # all, and a close in one really closes it. If nk gives each thread a
    # copy taken at creation, two things break that look nothing alike -- a
    # descriptor opened later is invisible to an existing thread, and a
    # socket closed by a worker stays open on the parent's copy, so the peer
    # never sees EOF and waits forever. That second one is exactly the shape
    # of a hung HTTP response.
    late_r, late_w = None, None
    seen = {}
    gate = threading.Event()
    done = threading.Event()

    def watcher():
        gate.wait(10)
        try:
            os.write(late_w, b"x")          # opened after this thread started
            seen["write"] = "visible"
        except OSError as e:
            seen["write"] = f"{errno.errorcode.get(e.errno, e.errno)}"
        done.set()

    threading.Thread(target=watcher, daemon=True).start()
    late_r, late_w = os.pipe()              # created after the thread exists
    gate.set()
    done.wait(10)
    step("threads-share-fd-table", seen.get("write") == "visible",
         f"late fd from another thread: {seen.get('write', 'no answer')}")
    shared_ok = seen.get("write") == "visible"

    # And the half that hangs a server: a close in one thread must be a real
    # close, so the peer sees EOF.
    if shared_ok:
        a_sock, b_sock = socket.socketpair()
        closed = threading.Event()

        def closer():
            a_sock.close()
            closed.set()

        threading.Thread(target=closer, daemon=True).start()
        closed.wait(10)
        b_sock.settimeout(5)
        try:
            eof = b_sock.recv(16) == b""
        except OSError as e:
            eof = False
            seen["eof"] = repr(e)
        step("close-in-thread-is-a-real-close", eof,
             seen.get("eof", "peer saw EOF" if eof else "peer never saw EOF"))
        b_sock.close()

    for fd in (late_r, late_w):
        if fd is not None:
            try:
                os.close(fd)
            except OSError:
                pass

    # 1. Is there an AF_INET stack at all? A kernel built without CONFIG_INET
    #    fails here with EAFNOSUPPORT -- Linux answering, not nk refusing.
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    except OSError as e:
        step("socket", False, repr(e))
        return 1
    step("socket", True)

    # 2. Is there a loopback interface to bind to? Present but down is the
    #    interesting failure: the address exists and bind refuses it.
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind((HOST, PORT))
    except OSError as e:
        step("bind", False, repr(e))
        return 2
    step("bind", True, f"{HOST}:{PORT}")
    s.close()

    # 3. UDP first, because it separates the two things a hung connect()
    #    could mean. A datagram over loopback exercises exactly the packet
    #    path -- loopback_xmit, netif_rx, the NET_RX softirq -- and none of
    #    the TCP state machine or its timers. If this works and connect()
    #    hangs, the packets are moving and TCP is the problem; if this hangs
    #    too, nothing is being delivered at all.
    try:
        a = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        a.bind((HOST, 7778))
        a.settimeout(10)
        b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        b.sendto(b"udp-ping", (HOST, 7778))
        got, _ = a.recvfrom(64)
        a.close()
        b.close()
    except OSError as e:
        step("udp", False, repr(e))
        return 3
    if got != b"udp-ping":
        step("udp", False, repr(got))
        return 3
    step("udp", True, "loopback delivers datagrams")

    # 3b. Does a timeout fire at all?
    #
    # Python implements a connect with a timeout as a non-blocking connect
    # followed by poll(), so a connect that ignores its own timeout is not
    # a TCP symptom -- it is poll() never returning. Every event loop in the
    # desktop sits in exactly this call, so it is worth its own step: poll a
    # descriptor that can never become readable and require it to come back
    # empty. If this hangs, nothing above it can be diagnosed.
    r, _w = os.pipe()                       # never written to
    t0 = time.monotonic()
    poller0 = select.poll()
    poller0.register(r, select.POLLIN)
    res0 = poller0.poll(2000)
    waited = time.monotonic() - t0
    os.close(r)
    os.close(_w)
    if res0 or waited < 1.0:
        step("poll-timeout", False, f"returned {res0!r} after {waited:.2f}s")
        return 3
    step("poll-timeout", True, f"expired after {waited:.2f}s")

    # What Linux itself thinks happened. When a connect fails the errno is
    # rarely the interesting part: ActiveOpens with no InSegs says the SYN
    # left and nothing came back; neither moving says it never left.
    def counters(tag):
        try:
            with open("/proc/net/snmp") as fh:
                lines = fh.read().split(chr(10))
            keys = vals = None
            for ln in lines:
                if ln.startswith("Tcp:") and keys is None:
                    keys = ln.split()
                elif ln.startswith("Tcp:"):
                    vals = ln.split()
                    break
            if keys and vals:
                want = ("ActiveOpens", "PassiveOpens", "AttemptFails",
                        "InSegs", "OutSegs", "RetransSegs", "CurrEstab")
                print(f"  tcp[{tag}] "
                      + " ".join(f"{k}={v}" for k, v in zip(keys, vals) if k in want),
                      flush=True)
        except OSError as e:
            print(f"  tcp[{tag}] unreadable: {e!r}", flush=True)
        try:
            with open("/proc/net/tcp") as fh:
                rows = [r for r in fh.read().split(chr(10))[1:] if r.strip()]
            print(f"  sockets[{tag}] {len(rows)}", flush=True)
            for r in rows[:4]:
                f = r.split()
                print(f"    local {f[1]} rem {f[2]} st {f[3]}", flush=True)
        except OSError:
            pass

    # 4. A self-connect with no threads anywhere.
    #
    # The threaded version hung in a way that could not be explained -- a
    # connect with a timeout ignoring it, on a kernel whose poll timeouts
    # demonstrably work. So this removes every variable at once: one thread,
    # a non-blocking connect that must return EINPROGRESS immediately, and
    # poll for writability. Nothing here can block, so whatever the state is,
    # it gets reported instead of hanging.
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind((HOST, PORT))
    srv.listen(8)
    srv.setblocking(False)
    step("raw-listen", True)

    counters("before")
    cli = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    cli.setblocking(False)
    err = cli.connect_ex((HOST, PORT))
    step("connect_ex", err in (0, errno.EINPROGRESS),
         f"errno {err} ({errno.errorcode.get(err, '?')})")
    counters("after-connect")

    poller = select.poll()
    poller.register(cli.fileno(), select.POLLOUT)
    ready = poller.poll(5000)
    step("connect-completes", bool(ready), repr(ready))
    counters("after-poll")
    if not ready:
        # The whole point: say what Linux thinks happened, then stop.
        return 3

    soerr = cli.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)
    step("so-error-clear", soerr == 0, f"SO_ERROR {soerr}")

    try:
        conn, _ = srv.accept()
    except OSError as e:
        step("accept", False, repr(e))
        return 3
    step("accept", True)

    cli.sendall(b"ping")
    p2 = select.poll()
    p2.register(conn.fileno(), select.POLLIN)
    if not p2.poll(5000):
        step("data", False, "nothing readable")
        return 3
    got = conn.recv(16)
    step("data", got == b"ping", repr(got))
    if got != b"ping":
        return 3
    cli.close()
    conn.close()
    srv.close()

    # 5. The whole nethosd shape: threaded server, real client, real response.
    srv = ThreadingHTTPServer((HOST, PORT), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    step("listen", True)

    try:
        c = socket.create_connection((HOST, PORT), timeout=10)
        c.sendall(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        data = b""
        while True:
            chunk = c.recv(4096)
            if not chunk:
                break
            data += chunk
        c.close()
    except OSError as e:
        step("request", False, repr(e))
        return 3

    if b"200 OK" not in data:
        step("request", False, repr(data[:80]))
        return 4
    body = data.split(b"\r\n\r\n", 1)[-1]
    try:
        parsed = json.loads(body)
    except ValueError:
        step("json", False, repr(body[:80]))
        return 5
    step("request", True, f"{len(data)} bytes")
    step("json", True, json.dumps(parsed))

    srv.shutdown()
    print("NETHOSD_SHAPE_OK", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
