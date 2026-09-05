The damage tracker and Kawase blur are adapted from Wayfire v0.9.0,
commit 15065ca1a2e3a596db3dbcba64823c5dee90f191, plugins/blur/.
https://github.com/WayfireWM/wayfire/tree/v0.9.0/plugins/blur

MIT licence, retained in LICENSE and blur.hpp. NETHOS adds the edge-refraction
shader, isolated plugin/config namespace, software-renderer guard, extra damage
padding for displacement, and protection against zero-size downsample buffers.
This plugin replaces blur, never runs alongside it. The build rejects other
Wayfire ABI series rather than shipping a binary that may crash the compositor.
