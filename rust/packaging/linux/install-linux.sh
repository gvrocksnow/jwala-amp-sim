#!/usr/bin/env bash
# Installs the Jwala standalone with its menu entry and icons.
#   ./install-linux.sh          for the current user (~/.local)
#   sudo ./install-linux.sh     for everyone (/usr/local)
#   ./install-linux.sh --uninstall   (with sudo if it was installed with sudo)
set -euo pipefail
cd "$(dirname "$0")"
if [ "$(id -u)" -eq 0 ]; then prefix=/usr/local; else prefix="$HOME/.local"; fi
bin="$prefix/bin"
apps="$prefix/share/applications"
hicolor="$prefix/share/icons/hicolor"
sizes="16 24 32 48 64 128 256 512"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin/jwala" "$apps/jwala.desktop"
    for n in $sizes; do rm -f "$hicolor/${n}x${n}/apps/jwala.png"; done
    echo "Jwala removed from $prefix"
else
    # build.sh names the binary `jwala`, the GitHub release zip `Jwala`
    src=jwala
    [ -f "$src" ] || src=Jwala
    install -Dm755 "$src" "$bin/jwala"
    # absolute Exec path, so the menu entry works even if $bin isn't on PATH
    mkdir -p "$apps"
    sed "s|^Exec=.*|Exec=$bin/jwala|" jwala.desktop > "$apps/jwala.desktop"
    for n in $sizes; do install -Dm644 "icons/jwala_$n.png" "$hicolor/${n}x${n}/apps/jwala.png"; done
    echo "Jwala installed to $prefix"
fi
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$hicolor" 2>/dev/null || true
command -v update-desktop-database >/dev/null && update-desktop-database "$apps" 2>/dev/null || true
