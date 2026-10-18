#!/bin/sh
# Boot ruOS in QEMU on the serial console (Ctrl-A X quits QEMU).
# Usage: tools/run-qemu.sh <bzImage> <initramfs> [config-file]
# The optional config file (KEY=VALUE lines, e.g. ANTHROPIC_API_KEY=...) is
# handed to the guest through fw_cfg; it is never written into the image.
set -eu

kernel=$1 initrd=$2 config=${3:-}

set -- -m 512M -smp 2 -nographic -no-reboot \
    -kernel "$kernel" -initrd "$initrd" \
    -append "console=ttyS0 quiet panic=-1" \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
    -device virtio-rng-pci

if [ -n "$config" ]; then
    set -- "$@" -fw_cfg name=opt/ruos/config,file="$config"
fi
if [ -w /dev/kvm ]; then
    set -- "$@" -enable-kvm -cpu host
fi

exec qemu-system-x86_64 "$@"
