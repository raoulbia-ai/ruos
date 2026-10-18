#!/bin/sh
# Build the ruOS root filesystem as a gzipped cpio initramfs.
# Usage: tools/mkinitramfs.sh <output.cpio.gz> <userland-bin-dir> <static-busybox>
set -eu

out=$1 bindir=$2 busybox=$3
repo=$(cd "$(dirname "$0")/.." && pwd)

if ! file -L "$busybox" | grep -q 'statically linked'; then
    echo "mkinitramfs: $busybox is not a static binary" >&2
    exit 1
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir -p "$stage"/bin "$stage"/sbin "$stage"/usr/bin "$stage"/usr/sbin \
         "$stage"/etc "$stage"/proc "$stage"/sys "$stage"/dev "$stage"/run \
         "$stage"/tmp "$stage"/root "$stage"/var/log
chmod 1777 "$stage"/tmp
chmod 700 "$stage"/root

# busybox and one symlink per applet it provides.
cp "$busybox" "$stage"/bin/busybox
"$busybox" --list-full | while read -r applet; do
    [ -e "$stage/$applet" ] || ln -s /bin/busybox "$stage/$applet"
done

# Our programs. /init is what the kernel runs from an initramfs.
install -m 0755 "$bindir"/ruos-init "$stage"/sbin/ruos-init
install -m 0755 "$bindir"/ruosd "$stage"/usr/sbin/ruosd
install -m 0755 "$bindir"/ai "$stage"/usr/bin/ai
ln -sf /sbin/ruos-init "$stage"/init
ln -sf /sbin/ruos-init "$stage"/sbin/init

cp -R "$repo"/rootfs/. "$stage"/

(cd "$stage" && find . | LC_ALL=C sort | cpio --quiet -o -H newc -R 0:0) | gzip -9 > "$out"
echo "mkinitramfs: wrote $out ($(du -h "$out" | cut -f1))"
