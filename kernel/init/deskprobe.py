"""The NETHOS desktop in a window, on nk.

nethos-view hosts the shell's pages in WebKitGTK. This is that, reduced to
what can be asserted: connect to the compositor, open a window, put a
WebKitWebView in it showing the real shell page off the disk, wait for the
load to finish, and say so.

Reports each step rather than a verdict. When this fails, which step it
reached is the whole of the useful output.
"""

import os
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
gi.require_version("WebKit", "6.0")
from gi.repository import GLib, Gdk, Gtk, WebKit  # noqa: E402

PAGE = os.environ.get("NETHOS_PAGE", "/mnt/nethos/shell/desktop.html")
DEADLINE = 60


def say(tag, detail=""):
    print(f"DESK_{tag}{' ' + detail if detail else ''}", flush=True)


class Desktop:
    def __init__(self):
        self.loaded = False
        self.failed = None
        self.window = None

    def build(self, app):
        self.window = Gtk.ApplicationWindow(application=app)
        self.window.set_default_size(1024, 768)
        self.window.set_title("NETHOS")

        self.view = WebKit.WebView()
        # The shell is local pages reading local files; without this the
        # file:// page cannot load its own CSS and JS siblings.
        s = self.view.get_settings()
        s.set_allow_file_access_from_file_urls(True)
        s.set_allow_universal_access_from_file_urls(True)
        self.view.connect("load-changed", self.on_load)
        self.view.connect("load-failed", self.on_fail)

        self.window.set_child(self.view)
        self.window.present()
        say("WINDOW_PRESENTED")

        uri = GLib.filename_to_uri(PAGE, None)
        say("LOADING", uri)
        self.view.load_uri(uri)

    def on_load(self, view, event):
        say("LOAD_EVENT", event.value_nick)
        if event == WebKit.LoadEvent.FINISHED:
            self.loaded = True
            say("LOAD_FINISHED", view.get_uri() or "")
            # The title comes from the page itself, so a title proves the
            # document was parsed rather than merely fetched.
            say("PAGE_TITLE", repr(view.get_title()))
            GLib.timeout_add(1500, self.settle)

    def on_fail(self, view, event, uri, error):
        self.failed = error.message
        say("LOAD_FAILED", f"{uri}: {error.message}")
        self.finish()
        return True

    def settle(self):
        # A frame after the load, so anything the page draws on first paint
        # has actually been through the compositor.
        say("SETTLED")
        say("DESKTOP_OK")
        self.finish()
        return False

    def finish(self):
        if self.window:
            self.window.close()
        app.quit()


def on_activate(a):
    d = Desktop()
    d.build(a)
    GLib.timeout_add_seconds(DEADLINE, lambda: (say("TIMEOUT"), a.quit())[1])


if __name__ == "__main__":
    display = Gdk.Display.open(os.environ.get("WAYLAND_DISPLAY") or None)
    if display is None:
        say("NO_DISPLAY", "compositor not reachable")
        sys.exit(2)
    say("DISPLAY", display.get_name())

    app = Gtk.Application(application_id="os.nethos.desk")
    app.connect("activate", on_activate)
    sys.exit(app.run([]))
