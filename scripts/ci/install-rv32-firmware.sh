#!/usr/bin/env bash
# Ubuntu's QEMU package omits RV32 OpenSBI; use QEMU's pinned upstream image.
set -euo pipefail

destination=${1:?Usage: bash install-rv32-firmware.sh QEMU_FIRMWARE_DIRECTORY}
firmware=opensbi-riscv32-generic-fw_dynamic.bin
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

curl --fail --silent --show-error --location --retry 3 \
    "https://raw.githubusercontent.com/qemu/qemu/v8.2.2/pc-bios/$firmware" \
    --output "$temporary/$firmware"
printf '%s  %s\n' \
    '997c7e351c9b3b361f4cfb0b8fa4bef2011005f3a30b8eb9b3cca91da8a2625a' \
    "$temporary/$firmware" | sha256sum --check -
install -D -m 0644 "$temporary/$firmware" "$destination/$firmware"
