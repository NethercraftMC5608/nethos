#!/bin/bash
# Linux implementation, run by build-installer-bundle.sh in a privileged builder.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
SRC=/src
WORK=/work/nethos-offline
SYSTEM="$WORK/root"
LIVE="$WORK/installer-root"
OUT="$SRC/build/installer-bundle"
ISO_TREE="$WORK/iso"
mkdir -p "$OUT" "$WORK"
exec > >(tee "$OUT/build.log") 2>&1
apt-get update -qq
apt-get install -y -qq python3 xorriso grub-pc-bin grub-efi-amd64-bin mtools \
    squashfs-tools cpio zstd kmod build-essential meson ninja-build pkg-config \
    wayfire-dev libwf-config-dev libwlroots-0.18-dev libglm-dev libgles2-mesa-dev \
    libgtk-4-dev libgtk4-layer-shell-dev libwebkitgtk-6.0-dev
cd "$SRC"
if [ "${NETHOS_REUSE_ROOT:-0}" != 1 ]; then
    [ ! -d "$SYSTEM/usr" ] || { echo 'Root exists; use --reuse-root or a new builder.'; exit 1; }
    python3 -u pkg/npkg_bootstrap.py "$SYSTEM" --arch amd64 \
        --set base --set system --set kernel --set desktop --set firmware \
        --set browser --set net --set installer --set installer-gui \
        --work "$WORK/cache" --keep-debs
fi
[ -f "$SYSTEM/usr/bin/wayfire" ] || { echo 'Full system root is incomplete'; exit 1; }
# Conversion failures must not silently make an installer with missing tools.
python3 - "$WORK/cache/packages" <<'PY'
import sys
sys.path.insert(0,'/src/pkg')
from npkg import Repository
from npkg_bootstrap import _seed_packages
repo=Repository('build',sys.argv[1]);repo.fetch_index()
seeds=_seed_packages(['base','system','kernel','desktop','firmware','browser','net','installer','installer-gui'],'amd64',None)
missing=[p for p in seeds if not repo.best(p.replace('{arch}','amd64'))]
if missing: raise SystemExit('Required packages missing: '+', '.join(missing))
PY
if [ ! -f "$LIVE/.bootstrap-complete" ]; then
    [ ! -d "$LIVE/usr" ] || { echo 'Incomplete installer root; inspect it before removing it.'; exit 1; }
    python3 -u pkg/npkg_bootstrap.py "$LIVE" --arch amd64 \
        --set base --set system --set kernel --set firmware --set net \
        --set installer --set installer-gui --repo "$WORK/cache/packages" \
        --work "$WORK/live-cache" --keep-debs
    touch "$LIVE/.bootstrap-complete"
fi
scripts/build-glass.sh
NETHOS_VIEW_OUT="$OUT/nethos-view-native-x86_64" payload/nethos-view-native/build.sh
strip --strip-debug "$OUT/nethos-view-native-x86_64"
# Refresh the desktop after bootstrap, so both paths install this checkout.
python3 - "$SYSTEM" <<'PY'
import sys
sys.path.insert(0,'/src/pkg')
from npkg_bootstrap import install_desktop
install_desktop(sys.argv[1],'/src/payload','neth','amd64')
PY
install -m755 "$OUT/nethos-view-native-x86_64" "$SYSTEM/usr/bin/nethos-view-native"
install -d "$SYSTEM/etc/nethos"
printf '{"architecture":"amd64","commit":"%s","kind":"full-system"}\n' \
    "$(git -C "$SRC" rev-parse HEAD)" > "$SYSTEM/etc/nethos/system-image.json"

# The installer carries its own pinned source/native host for the online path.
mkdir -p "$LIVE/usr/share/nethos"
cp -a "$SRC/pkg" "$LIVE/usr/share/nethos/"
cp -a "$SRC/payload" "$LIVE/usr/share/nethos/"
mkdir -p "$LIVE/usr/share/nethos/installer"
cp -a "$SRC/payload/installer/gui" "$LIVE/usr/share/nethos/installer/"
install -m755 "$OUT/nethos-view-native-x86_64" "$LIVE/usr/share/nethos/payload/bin/nethos-view-native"
for binary in "$SRC"/payload/bin/*; do
    [ -f "$binary" ] && install -m755 "$binary" "$LIVE/usr/bin/"
done
mkdir -p "$LIVE/usr/share/fonts/truetype/nethos"
cp "$SRC/payload/lib/fonts/SpaceGrotesk[wght].ttf" "$LIVE/usr/share/fonts/truetype/nethos/"
cp "$SRC/payload/lib/fonts/OFL-SpaceGrotesk.txt" "$LIVE/usr/share/fonts/truetype/nethos/"
mkdir -p "$LIVE/etc/nethos" "$LIVE/etc/systemd/system/multi-user.target.wants"
touch "$LIVE/etc/nethos/live"
install -m644 "$SRC/payload/systemd/nethos-installer-gui.service" "$LIVE/etc/systemd/system/"
ln -sf ../nethos-installer-gui.service "$LIVE/etc/systemd/system/multi-user.target.wants/nethos-installer-gui.service"
# Only the live root starts the installer. The companion root boots the desktop.
rm -f "$LIVE/etc/systemd/system/getty.target.wants/getty@tty1.service"
mkdir -p "$LIVE/etc/initramfs-tools/conf.d"
printf 'BOOT=live\nMODULES=most\nCOMPRESS=xz\n' > "$LIVE/etc/initramfs-tools/conf.d/nethos-live"

prepare_root() {
    local root=$1
    mkdir -p "$root"/{dev,proc,sys,run,tmp,etc/ssl/certs,etc/npkg}
    mount --rbind /dev "$root/dev"
    mount --make-rslave "$root/dev"
    mount -t proc proc "$root/proc"
    mount --rbind /sys "$root/sys"
    mount --make-rslave "$root/sys"
    cp -L /etc/resolv.conf "$root/etc/resolv.conf"
    chroot "$root" /bin/bash -euo pipefail <<'CHROOT'
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
ldconfig
update-ca-certificates
fc-cache -f
if command -v glib-compile-schemas >/dev/null; then glib-compile-schemas /usr/share/glib-2.0/schemas; fi
mkdir -p /var/lib/dbus
: > /etc/machine-id
ln -sf /etc/machine-id /var/lib/dbus/machine-id
systemctl enable NetworkManager
systemctl mask systemd-firstboot.service
kver=$(ls /lib/modules | sort -V | tail -1)
depmod "$kver"
update-initramfs -c -k "$kver"
CHROOT
    umount -R "$root/dev"
    umount "$root/proc"
    umount -R "$root/sys"
    # Build paths must never become the installed package source.
    printf '{"repos":[{"name":"nethos","url":"https://moddl.app"}]}\n' > "$root/etc/npkg/repos.json"
}
trap 'for r in "$LIVE" "$SYSTEM"; do umount -Rl "$r/dev" "$r/proc" "$r/sys" 2>/dev/null || true; done' EXIT
prepare_root "$SYSTEM"
mkdir -p "$LIVE/boot"
cp -a "$SYSTEM/boot/." "$LIVE/boot/"
prepare_root "$LIVE"

# Check executable links/ELF loading in the actual relaid-out root.
chroot "$LIVE" python3 -c 'import tkinter; print("Tk", tkinter.TkVersion)'
chroot "$LIVE" /usr/lib/xorg/Xorg -version
chroot "$LIVE" fc-match 'DejaVu Sans'
kver=$(ls "$LIVE/lib/modules" | sort -V | tail -1)
for driver in i915 amdgpu nouveau virtio_gpu simpledrm; do
    chroot "$LIVE" modinfo -k "$kver" "$driver" >/dev/null 2>&1 || \
        grep -q "CONFIG_DRM_$(echo "$driver" | tr a-z A-Z)=y" "$LIVE/boot/config-"* || \
        { echo "Missing graphics driver: $driver"; exit 1; }
done
mkdir -p "$ISO_TREE/live" "$ISO_TREE/boot/grub"
kver=$(ls "$LIVE/lib/modules" | sort -V | tail -1)
cp "$LIVE/boot/vmlinuz-$kver" "$ISO_TREE/live/vmlinuz"
cp "$LIVE/boot/initrd.img-$kver" "$ISO_TREE/live/initrd.img"
# The boot files already sit outside squashfs; the installer never copies itself.
rm -f "$LIVE/boot/"*
for root in "$LIVE" "$SYSTEM"; do
    find "$root/usr/share/doc" -type f ! -name copyright -delete 2>/dev/null || true
    rm -rf "$root/usr/share/man" "$root/usr/share/info" "$root/var/cache/npkg" "$root/var/cache/apt"
    find "$root/var/log" -type f -exec truncate -s 0 {} +
    rm -f "$root/etc/ssh/ssh_host_"*
done
# A separate full-system file keeps the boot installer below its hard limit.
mksquashfs "$SYSTEM" "$OUT/nethos-system-x86_64.squashfs" -noappend -comp zstd -Xcompression-level 12 -processors 4
mksquashfs "$LIVE" "$ISO_TREE/live/filesystem.squashfs" -noappend -comp xz -b 1M -processors 4 -e .bootstrap-complete
cat > "$ISO_TREE/boot/grub/grub.cfg" <<'GRUB'
set timeout=8
set default=0
insmod all_video
insmod gfxterm
set gfxmode=1024x768,800x600,auto
set gfxpayload=keep
terminal_output gfxterm
menuentry 'NETHOS installer — minimal 2D graphics' {
 linux /live/vmlinuz boot=live components quiet console=tty0
 initrd /live/initrd.img
}
menuentry 'NETHOS installer — safe firmware graphics' {
 linux /live/vmlinuz boot=live components nomodeset quiet console=tty0
 initrd /live/initrd.img
}
menuentry 'NETHOS installer — text / troubleshooting' {
 linux /live/vmlinuz boot=live components nethos.installer=text console=tty0 console=ttyS0,115200
 initrd /live/initrd.img
}
GRUB
grub-mkrescue -o "$OUT/nethos-installer-x86_64.iso" "$ISO_TREE"
bytes=$(stat -c%s "$OUT/nethos-installer-x86_64.iso")
if [ "$bytes" -ge 500000000 ]; then
    echo "Installer exceeds 500 MB: $bytes bytes. Do not publish this build."; exit 1
fi
(cd "$OUT"; sha256sum nethos-installer-x86_64.iso > nethos-installer-x86_64.iso.sha256; sha256sum nethos-system-x86_64.squashfs > nethos-system-x86_64.squashfs.sha256)
python3 - "$SYSTEM" "$LIVE" "$OUT" <<'PY'
import sys,json
from pathlib import Path
sys.path.insert(0,'/src/pkg')
from npkg import Database
for root,label in [(sys.argv[1],'system'),(sys.argv[2],'installer')]:
    packages=Database(root).installed()
    Path(sys.argv[3],label+'-packages.txt').write_text(''.join(f'{name}\t{pkg.version}\n' for name,pkg in sorted(packages.items())))
    print(label,len(packages),'packages')
PY
ls -lh "$OUT/"*.iso "$OUT/"*.squashfs
printf 'Installer: %s bytes, below the 500,000,000-byte limit.\n' "$bytes"
