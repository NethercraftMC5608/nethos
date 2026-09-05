# NETHOS desktop materials

The default is dark smoked glass with slate wallpaper; light appearance uses
the same spacing, radii and material roles. Shell controls, widget surrounds,
NETHOS window surfaces and Wayfire decoration colours share the appearance.
Reading surfaces are denser. Opaque third-party application content stays opaque.
The NETHOS switcher has fixed cards, window titles, application names and a focus
ring, without clouds or a moving hover carousel. Wayfire retains its native
fast-switcher keyboard binding; sway/Hyprland use the NETHOS overlay.

## Real compositor rendering

`payload/wayfire/glass` builds **nethos-glass**, a Wayfire **0.9.x** module based
on the MIT-licensed v0.9.0 blur renderer (exact revision in `UPSTREAM.md`). It
replaces `blur`; the two are never enabled together.

The compositor copies the scene below each translucent client, applies Kawase
blur, and bends backdrop coordinates at alpha boundaries. A restrained top rim
adds light. The client texture is sampled at its original coordinates; opaque
text and content are returned unchanged. There is no time uniform or idle
animation. Transparent outside pixels stay transparent. Alpha boundaries support
rounded widgets and irregular notification shapes without a fullscreen rectangle
being treated as one piece of glass.

The upstream scene integration expands damage and restores framebuffer padding.
Additional logical-pixel padding covers the displacement and alpha-gradient
samples. Background displacement uses the same matrix as the backdrop, and
sampling is clamped to the framebuffer. Downsample dimensions are clamped for
tiny/transient surfaces. This is a stylised edge-refraction material, not a
physical simulation of glass or Apple's proprietary renderer.

Wallpaper now occupies a separate background layer. Widgets remain on the
transparent desktop layer, giving the compositor an actual backdrop to sample.
Widgets embedded in that layer share the host's events and appearance; they do
not each occupy an indefinitely open HTTP connection.

## Build and install

On Debian trixie **on the target CPU**, install these development packages:

```sh
sudo apt install build-essential meson ninja-build pkg-config wayfire-dev \
  libwf-config-dev libwlroots-0.18-dev libglm-dev libgles2-mesa-dev
scripts/build-glass.sh
sudo payload/install-nethos.sh --files-only
```

The build stages a matching-architecture module in
`payload/wayfire/built/<uname -m>/`. Generated binaries are ignored by Git.
The file installer and npkg image bootstrap copy that module into
`/usr/lib/nethos/wayfire`; metadata goes under `/usr/share/nethos/wayfire`.
Build once per target architecture before assembling an image. The build rejects
other Wayfire ABI series. The launcher also checks the installed Wayfire series.
A build for a newer Wayfire needs a source port and a new validation pass.

Log out and back in after installation. `nethos-wayfire` preserves the user's
base config and generates `$XDG_RUNTIME_DIR/nethos/wayfire.ini`. It selects the
custom module when available, standard blur when it is missing, or no effect
when reduced transparency is selected. A CPU-only machine runs the existing sway
fallback: Wayfire 0.9 requires a DRM GLES renderer even if `WLR_RENDERER=pixman`
is set. The module also refuses llvmpipe/softpipe unless its explicit test override
is enabled. Do not enable that override for the normal session.

The selected startup mode is recorded in `$XDG_RUNTIME_DIR/nethos/glass-mode`.
Wayfire logs `nethos-glass: backdrop refraction on <renderer>` only after its
renderer check. The recorded mode is configuration intent, not proof of GPU
performance or successful shader execution.

Theme changes update apps and shell surfaces without reloading their documents.
The generated config updates decoration colours without hot-swapping plugins.
Auto uses the platform's explicit light preference, otherwise NETHOS dark.
Reduced transparency makes web surfaces solid immediately; restarting the session
removes the compositor effect. Third-party chrome follows the selected palette;
third-party interiors remain under their toolkit's control.

## Validation

Completed in the development environment:

- Compiled and linked the module against Debian Wayfire 0.9.0/wf-config 0.9.0.
- Compiled the native GTK4/WebKit host with the capability/theme bridge changes.
  Existing format-truncation warnings remain in the spec parser and settle helper.
- Ran `tests/glass-render.cpp` using a real Mesa GLES llvmpipe context: 2,256
  edge channels changed under refraction; opaque pixels, transparent exterior
  and central content stayed unchanged. A minimum-size surface also rendered.
- Configuration tests cover multiline plugin lists, preservation of bindings,
  mutually exclusive effects, fallbacks and palette refresh without plugin swaps.
- SDK tests cover shared host/iframe events, browser EventSource fallback,
  live appearance and the app reload opt-out.
- Python/JavaScript/shell syntax checks and dark/light browser layout review.
  `tools/glass-review.html` uses the production CSS and labels its preview limits.

To repeat the shader check on Linux:

```sh
g++ -std=c++17 tests/glass-render.cpp -o /tmp/glass-render -lEGL -lGLESv2
EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 /tmp/glass-render
python3 -m unittest discover -s tests -p 'test_*.py'
node tests/test-sdk.cjs
```

**GPU session validation remains required.** The container has no DRM device;
Wayfire exits before loading any plugin there. No claim is made of an end-to-end
GPU boot, frame times, idle GPU use, mixed-DPI output correctness or screencast
compatibility. On GPU Linux, exercise overlapping moving windows, scrolling
behind glass, output scale/rotation changes, fullscreen transitions, popups and
screencasts. Compare frame times with standard blur, and check for stale pixels
at damaged edges. The ARM Mac VM is not a substitute for that hardware test.

## Research and provenance

- [Apple material guidance](https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass):
  separate navigation/control material from readable content.
- [Wayfire v0.9.0 blur renderer](https://github.com/WayfireWM/wayfire/tree/v0.9.0/plugins/blur):
  real scene sampling, transforms and damage management; MIT licence retained.
- [QEMU virtio-gpu](https://www.qemu.org/docs/master/system/devices/virtio/virtio-gpu.html):
  2D and accelerated GPU backends are distinct capabilities.
