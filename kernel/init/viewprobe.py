"""How far does nethos-view's own stack get on nk?

nethos-view is Python, and its first forty lines are the whole question:
`import gi`, GTK 4.0, Gdk 4.0, WebKit 6.0, and gtk4-layer-shell when a
compositor offers one. That is a different stack from the C probe that
proved WebKit loads -- gobject-introspection reads typelibs at runtime and
dlopens the libraries they name, so it exercises paths a linked binary never
touches.

Walks forward one step at a time and reports each, rather than importing
everything and printing a verdict: the useful output of a run that fails is
which step it reached.
"""

import os
import sys
import traceback

STEPS = []


def step(name, fn):
    """Run one step. A failure is reported and does not stop the walk --
    later steps often still say something useful about why."""
    try:
        detail = fn()
        print(f"VIEW_OK   {name}{' ' + str(detail) if detail else ''}", flush=True)
        STEPS.append((name, True))
        return True
    except Exception as e:                                   # noqa: BLE001
        print(f"VIEW_FAIL {name}: {type(e).__name__}: {e}", flush=True)
        STEPS.append((name, False))
        return False


def _import_gi():
    import gi
    return gi.__version__ if hasattr(gi, "__version__") else "present"


def _require():
    import gi
    gi.require_version("Gtk", "4.0")
    gi.require_version("Gdk", "4.0")
    gi.require_version("WebKit", "6.0")
    return "Gtk 4.0, Gdk 4.0, WebKit 6.0"


def _repository():
    from gi.repository import Gdk, GLib, Gtk, WebKit
    return (f"glib {GLib.MAJOR_VERSION}.{GLib.MINOR_VERSION} "
            f"gtk {Gtk.MAJOR_VERSION}.{Gtk.MINOR_VERSION} "
            f"webkit {WebKit.get_major_version()}.{WebKit.get_minor_version()}"
            f".{WebKit.get_micro_version()} "
            f"gdk={Gdk._namespace}")


def _layershell():
    import gi
    gi.require_version("Gtk4LayerShell", "1.0")
    from gi.repository import Gtk4LayerShell as LayerShell
    return LayerShell.__name__


def _display():
    # The first thing that needs something outside this process. Without a
    # compositor there is no display to open, and that is a fact about the
    # machine rather than a fault -- report it plainly.
    from gi.repository import Gdk
    d = Gdk.Display.open(os.environ.get("WAYLAND_DISPLAY") or None)
    if d is None:
        raise RuntimeError("no display (expected without a compositor)")
    return d.get_name()


def _webview_type():
    # Constructing a WebView needs a display; asking for its GType does not.
    # It proves the class initialised, which is most of WebKit coming up.
    from gi.repository import WebKit
    return WebKit.WebView.__gtype__.name


def main():
    print(f"  view: python {sys.version.split()[0]}", flush=True)
    if not step("import gi", _import_gi):
        return 1
    if not step("require_version", _require):
        return 2
    if not step("from gi.repository", _repository):
        return 3
    step("webview gtype", _webview_type)
    step("layer-shell", _layershell)
    step("open display", _display)

    ok = [n for n, good in STEPS if good]
    print(f"VIEW_REACHED {len(ok)}/{len(STEPS)}", flush=True)
    # The bindings loading is the milestone this probe exists for; a display
    # needs a compositor and its absence is not a failure of the stack.
    if dict(STEPS).get("from gi.repository"):
        print("VIEW_BINDINGS_OK", flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:                                        # noqa: BLE001
        traceback.print_exc()
        sys.exit(9)
