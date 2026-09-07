/* M3 step B: slate framebuffer holder (folio of fbtest.c).
 *
 * Fills /dev/fb0 with the shell's dark-slate pixel (#14181f: the first stop
 * of the slate wallpaper gradient, payload/shell/style.css:1229), modesets
 * via FBIOPUT_VSCREENINFO + FBIOPAN_DISPLAY exactly like fbtest.c, prints
 * M3_SPIN_READY, and sleeps forever so the host can screendump the scanout
 * through the QEMU monitor. Deliberately through write() rather than mmap(),
 * for fbtest's reason: a DRM dumb buffer has to be mapped and device shared
 * mappings are refused on nk. Built static by scripts/build-wl-test.sh m3fb.
 */
#include <fcntl.h>
#include <linux/fb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

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

	/* Flat slate #14181f in xrgb byte order (B=0x1f G=0x18 R=0x14):
	 * a screendump whose mean colour is this value can only have come
	 * from this program. fbtest's gradient proves drawing; this proves
	 * the shell's own background byte reaching the scanout. */
	for (y = 0; y < var.yres; y++) {
		unsigned int x;

		for (x = 0; x < var.xres; x++) {
			unsigned char *px = row + x * (var.bits_per_pixel / 8);

			px[0] = 0x1f; /* blue  */
			px[1] = 0x18; /* green */
			px[2] = 0x14; /* red   */
			if (var.bits_per_pixel == 32)
				px[3] = 0x00;
		}
		if (write(fd, row, fix.line_length) != (ssize_t)fix.line_length) {
			perror("write");
			return 4;
		}
	}
	printf("fb: drew %u lines M3_SLATE\n", var.yres);

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
