#!/usr/bin/env bash
# Explicitly invoked provisioning helper; never elevates privileges itself.
set -euo pipefail
if [[ $(uname -s) != Linux || ! -r /etc/os-release ]]; then
  echo 'Debian or Ubuntu is required.' >&2; exit 1
fi
. /etc/os-release
case "${ID}:${VERSION_ID}" in
  debian:12|debian:13|ubuntu:22.04|ubuntu:24.04) ;;
  *) echo "Unsupported build host: ${ID} ${VERSION_ID}" >&2; exit 1 ;;
esac
packages=(build-essential pkg-config clang cmake curl ca-certificates git python3
  libfontconfig1-dev libfreetype6-dev libssl-dev libasound2-dev
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
  libvulkan-dev libegl-dev libgl-dev
  dpkg-dev binutils desktop-file-utils xdg-utils xdg-user-dirs libglib2.0-bin
  xdg-desktop-portal xdg-desktop-portal-gtk dbus-x11 fonts-noto-cjk
  mesa-vulkan-drivers)
if [[ ${1:-} == --print ]]; then
  printf '%s\n' "${packages[@]}"; exit 0
fi
if [[ $# != 0 ]]; then echo "Usage: $0 [--print]" >&2; exit 1; fi
if [[ $EUID != 0 ]]; then
  echo 'Run this dependency installer with sudo after reviewing it, or use --print to list packages.' >&2
  exit 1
fi
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends "${packages[@]}"
