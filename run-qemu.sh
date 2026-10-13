#!/bin/bash
# run-qemu.sh — boot the OpenWrt image in QEMU.
#
# Usage:
#   ./run-qemu.sh [path/to/*-combined.img.gz]
#
# Without an argument it uses the *-combined.img.gz lying next to this script
# (the CI artifact "openwrt-x86-64-image" has both in one folder).
#
# After boot: ssh root@localhost -p 2222
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
IMG_GZ="${1:-}"
if [ -z "$IMG_GZ" ]; then
    for f in "$HERE"/*-combined.img.gz; do
        [ -f "$f" ] && IMG_GZ="$f" && break
    done
fi
[ -n "$IMG_GZ" ] && [ -f "$IMG_GZ" ] || { echo "usage: $0 [path/to/*-combined.img.gz]" >&2; exit 1; }
WORKDIR="$(mktemp -d)"
IMG="${WORKDIR}/openwrt.img"

echo "== Decompressing image =="
# trailing padding makes gunzip exit 2 with a warning although the data is fine
gunzip -c "$IMG_GZ" > "$IMG" || [ $? -eq 2 ]

echo "== Starting QEMU (Ctrl-A X to quit, serial console in this terminal) =="
qemu-system-x86_64 \
    -m 256 \
    -M pc \
    -drive file="${IMG}",if=virtio,format=raw \
    -netdev user,id=net0,net=192.168.1.0/24,host=192.168.1.2,hostfwd=tcp::2222-192.168.1.1:22 \
    -device virtio-net-pci,netdev=net0 \
    -nographic
