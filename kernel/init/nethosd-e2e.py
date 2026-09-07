"""First nethosd end to end on nk: the real daemon, unmodified, over loopback.

Same shape as netprobe step 5 but with payload/nethosd/nethosd.py itself,
not a miniature: import it, run main() in a thread, GET /api/status over
TCP, check the JSON. nethosd's background threads (compositor loop, tray,
snapper, watch) all degrade to sleeps when there is no compositor, so the
only gate is ThreadingHTTPServer on 127.0.0.1:7777 -- fd table (#12, done),
loopback, threads, signals.

Known risk: #16 (poll wedges after repeated polls). One request, not a
loop, so a wedge here says the bug fires on the first serve cycle too.
"""

import json
import os
import socket
import sys
import threading
import time

sys.path.insert(0, "/")

HOST, PORT = "127.0.0.1", 7777


def step(name, ok, detail=""):
    print(f"{'NETHOSD_OK  ' if ok else 'NETHOSD_FAIL'} {name}{' ' + detail if detail else ''}",
          flush=True)
    return ok


def main():
    import nethosd

    step("import", True, "nethosd.main present: %s" % hasattr(nethosd, "main"))
    t0 = time.monotonic()
    print("NETHOSD_STARTED t=%.2f" % t0, flush=True)

    mode = os.environ.get("NETHOSD_E2E_MODE", "full")
    if mode == "import-only":
        # Opus decisive experiment (#17): import + exercise nethosd.status()
        # on THIS thread, spawn nothing. Survival = thread path guilty;
        # fault = threads innocent.
        d = nethosd.status()
        step("status-direct", True, "keys: %s" % ",".join(sorted(d.keys())))
        print("NETHOSD_IMPORT_ONLY_OK", flush=True)
        return 0
    print("NETHOSD_MODE full, spawning daemon thread", flush=True)

    threading.Thread(target=nethosd.main, daemon=True).start()
    print("NETHOSD_THREAD_SPAWNED t=%.2f" % time.monotonic(), flush=True)
    print("NETHOSD_MODE full t=%.2f" % time.monotonic(), flush=True)

    # Short sleeps with heartbeats, not one long sleep: if nk timers wedge
    # (#16), we see exactly which sleep never returns instead of silence.
    for i in range(6):
        time.sleep(0.5)
        print("NETHOSD_SLEEP %d/6 t=%.2f" % (i + 1, time.monotonic() - t0), flush=True)

    # Is anything listening?
    print("NETHOSD_CONNECTING t=%.2f" % (time.monotonic() - t0), flush=True)
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(10)
    try:
        s.connect((HOST, PORT))
    except OSError as e:
        step("listen", False, repr(e))
        return 1
    step("listen", True, f"{HOST}:{PORT} accepting")
    s.close()

    # The real request, raw socket so the client has no dependencies.
    try:
        c = socket.create_connection((HOST, PORT), timeout=10)
        c.sendall(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\n"
                  b"Connection: close\r\n\r\n")
        data = b""
        while True:
            chunk = c.recv(4096)
            if not chunk:
                break
            data += chunk
        c.close()
    except OSError as e:
        step("request", False, repr(e))
        return 2

    if b"200 OK" not in data:
        step("request", False, repr(data[:80]))
        return 3
    first = data.split(b"\r\n", 1)[0]
    if b"HTTP/1.1 200" not in first and b"HTTP/1.0 200" not in first:
        step("request", False, "not HTTP/1.x 200: %r" % (first[:60],))
        return 3
    step("statusline", True, first.decode("ascii", "replace"))
    body = data.split(b"\r\n\r\n", 1)[-1]
    try:
        parsed = json.loads(body)
    except ValueError:
        step("json", False, repr(body[:120]))
        return 4
    step("request", True, f"{len(data)} bytes")
    keys = sorted(parsed.keys())
    ok = "kernel" in parsed and "uptime" in parsed and "mem" in parsed
    step("status-shape", ok, "keys: %s" % ",".join(keys))
    if not ok:
        return 5
    print("NETHOSD_STATUS_OK kernel=%s uptime=%s" % (parsed["kernel"], parsed["uptime"]),
          flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
