/* M3 step B+C: shell-background framebuffer holder (folio of fbtest.c).
 *
 * Fills /dev/fb0 with the shell's dark-slate wallpaper colours
 * (payload/shell/style.css:1225-1230), modesets via FBIOPUT_VSCREENINFO +
 * FBIOPAN_DISPLAY exactly like fbtest.c, prints M3_SPIN_READY, and sleeps
 * forever so the host can screendump the scanout through the QEMU monitor.
 * Deliberately through write() rather than mmap(), for fbtest's reason: a
 * DRM dumb buffer has to be mapped and device shared mappings are refused
 * on nk. Built static by scripts/build-wl-test.sh m3fb.
 *
 * Step B drew the flat first stop (#14181f) and proved the byte path.
 * Step C draws the full linear layer as a vertical gradient through its
 * three exact stops (#14181f 0%, #11151c 60%, #0e1116 100%): vertical is an
 * approximation of the CSS 155deg angle, but every stop byte is exact and
 * scripts/m3-check.py --gradient recomputes the same integer ramp, so the
 * screendump comparison is still byte-level, now non-uniform.
 */
#include <fcntl.h>
#include <linux/fb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* One row's colour (R,G,B) on the piecewise-linear ramp through the slate
 * stops. Integer math, thousandths of height, k in 0..256 per segment --
 * m3-check.py mirrors this formula exactly. */
static void row_color(unsigned int y, unsigned int h, unsigned char *rgb)
{
	static const unsigned char stops[3][3] = {
		{ 0x14, 0x18, 0x1f }, /* #14181f at 0.00 */
		{ 0x11, 0x15, 0x1c }, /* #11151c at 0.60 */
		{ 0x0e, 0x11, 0x16 }, /* #0e1116 at 1.00 */
	};
	unsigned int t = h > 1 ? (y * 1000) / (h - 1) : 0;
	const unsigned char *a = stops[0], *b = stops[1];
	unsigned int lo = 0, hi = 600, k, c;

	if (t >= 600) {
		a = stops[1];
		b = stops[2];
		lo = 600;
		hi = 1000;
	}
	k = ((t - lo) * 256) / (hi - lo);
	for (c = 0; c < 3; c++)
		rgb[c] = (unsigned char)((a[c] * (256 - k) + b[c] * k + 128) / 256);
}

int main(void)
{
	struct fb_var_screeninfo var;
	struct fb_fix_screeninfo fix;
	unsigned char *row;
	int fd;
	unsigned int y;

	fd = open("/dev/fb0", O_RDWR);
	if (fd < 0) {
		perror("open /dev/fb0");
		return 1;
	}
	if (ioctl(fd, FBIOGET_VSCREENINFO, &var) < 0 ||
	    ioctl(fd, FBIOGET_FSCREENINFO, &fix) < 0) {
		perror("ioctl");
		return 2;
	}
	printf("fb: %ux%u at %u bpp, %u bytes per line\n",
	       var.xres, var.yres, var.bits_per_pixel, fix.line_length);

	row = malloc(fix.line_length);
	if (row == NULL)
		return 3;

	/* The slate linear layer as a vertical gradient through its three exact
	 * stops (non-uniform: a flat black frame fails this check, as does
	 * fbtest's red-tinted gradient). Byte order on the wire is xrgb:
	 * px = (B, G, R), matching fbtest's layout. */
	for (y = 0; y < var.yres; y++) {
		unsigned int x;
		unsigned char rgb[3];

		row_color(y, var.yres, rgb);
		for (x = 0; x < var.xres; x++) {
			unsigned char *px = row + x * (var.bits_per_pixel / 8);

			px[0] = rgb[2]; /* blue  */
			px[1] = rgb[1]; /* green */
			px[2] = rgb[0]; /* red   */
			if (var.bits_per_pixel == 32)
				px[3] = 0x00;
		}
		if (write(fd, row, fix.line_length) != (ssize_t)fix.line_length) {
			perror("write");
			return 4;
		}
	}
	printf("fb: drew %u lines M3_SLATE_GRADIENT\n", var.yres);

	/* Turn the display on, like fbtest: writing fills a shadow buffer and
	 * nothing is scanned out until a mode is set on the CRTC. */
	var.activate = FB_ACTIVATE_NOW;
	if (ioctl(fd, FBIOPUT_VSCREENINFO, &var) < 0)
		perror("fb: FBIOPUT_VSCREENINFO");
	else
		printf("fb: mode set, display should be active\n");

	if (ioctl(fd, FBIOPAN_DISPLAY, &var) < 0)
		perror("fb: FBIOPAN_DISPLAY");

	close(fd);
	/* Stay up: the monitor screendump needs a live machine, and nk's
	 * init exiting powers it down (measured: the unix monitor socket
	 * vanishes with the process, so a draw-then-exit probe cannot be
	 * screenshotted). */
	printf("M3_SPIN_READY\n");
	fflush(stdout);
	for (;;)
		sleep(5);
	return 0;
}
