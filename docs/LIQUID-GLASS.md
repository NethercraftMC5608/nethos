# Desktop material implementation and remaining renderer work

The desktop now defaults to dark smoked glass and slate wallpaper, with the
existing light choice preserved. Alt+Tab has no cloud geometry. Shared shell
surfaces use the same fill, edge and shadow tokens. Metal is opt-in, and CSS
backdrop blur requires the hardware-rendering flag. Native GTK frames retain
alpha so Wayfire can blur the scene behind them without fading their text.
Wayfire decorations have the matching dark palette; they currently require a
session restart, and do not yet follow live theme changes.

## Research

- Apple separates navigation/control material from readable content:
  https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass
- Wayfire's real backdrop blur implementation is the renderer integration point:
  https://github.com/WayfireWM/wayfire/blob/master/plugins/blur/blur.cpp
- QEMU distinguishes the 2D virtio GPU from accelerated backends:
  https://www.qemu.org/docs/master/system/devices/virtio/virtio-gpu.html

## Not implemented: compositor refraction

This change does not implement optical backdrop distortion. CSS highlights
cannot sample pixels belonging to another client. A production implementation
needs a version-pinned Wayfire extension and Linux compilation/validation:

1. Capture the scene below each material region before drawing its client.
2. Blur that backdrop once, caching unchanged regions. Expand damage for the
   blur and maximum displacement; invalidate on underlying window movement.
3. Use a rounded-rectangle distance field and edge normal to displace backdrop
   sampling near the rim. Keep central content undistorted, and apply a subtle
   top-lit highlight. Clip sampling at output edges and handle output scale.
4. Composite client content at its original alpha. Never fade an entire opaque
   third-party window to manufacture glass. Toolkit interiors need cooperation.
5. Expose material regions from the shell/native host so full-screen transparent
   overlays do not incur a full-screen effect. Disable scanout only while needed.
6. Unify decoration and shell material parameters, including live theme updates.

Validate on GPU Linux with overlapping windows, scrolling behind glass, mixed
DPI outputs, fullscreen transitions, dialogs and screencasts. Measure frame time
and idle GPU use; preserve a non-distorting reduced-effect mode. The ARM VM is a
functional test target, not evidence of GPU performance. Do not hot-swap the
Wayfire decoration plugin in a live working session.
