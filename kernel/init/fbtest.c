/* Draw on nk's framebuffer.
 *
 * Deliberately through write() rather than mmap(). A DRM dumb buffer has to
 * be mapped, and a shared file-backed mapping is not something nk can do yet;
 * fbdev's write path wants nothing but a file descriptor. That is the whole
 * reason CONFIG_DRM_FBDEV_EMULATION is switched on -- it is the shortest
 * honest route from "the driver initialised" to "there are pixels".
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
	int fd, y;

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

	/* A vertical gradient, so a screenshot shows something that could not
	 * have been there by accident. */
	for (y = 0; y < (int)var.yres; y++) {
		unsigned int x;

		for (x = 0; x < var.xres; x++) {
			unsigned char *px = row + x * (var.bits_per_pixel / 8);

			px[0] = (unsigned char)(x * 255 / var.xres);   /* blue  */
			px[1] = (unsigned char)(y * 255 / var.yres);   /* green */
			px[2] = 0x40;                                  /* red   */
			if (var.bits_per_pixel == 32)
				px[3] = 0xff;
		}
		if (write(fd, row, fix.line_length) != (ssize_t)fix.line_length) {
			perror("write");
			return 4;
		}
	}
	printf("fb: drew %u lines\n", var.yres);

	/* Turn the display on.
	 *
	 * Writing pixels fills a shadow buffer and nothing scans it out until
	 * a mode has been set on the CRTC -- QEMU says "Display output is not
	 * active" until then. FBIOPUT_VSCREENINFO is how fbdev asks for that:
	 * the DRM fbdev helper turns it into drm_fb_helper_set_par, which
	 * does the modeset. fbcon does the same thing on an ordinary Linux,
	 * and dragging fbcon in costs the system console.
	 */
	var.activate = FB_ACTIVATE_NOW;
	if (ioctl(fd, FBIOPUT_VSCREENINFO, &var) < 0)
		perror("fb: FBIOPUT_VSCREENINFO");
	else
		printf("fb: mode set, display should be active\n");

	/* And ask for it to be sent to the device: the helper flushes damage
	 * from a worker, so a program that exits immediately can beat it. */
	if (ioctl(fd, FBIOPAN_DISPLAY, &var) < 0)
		perror("fb: FBIOPAN_DISPLAY");

	close(fd);
	return 0;
}
