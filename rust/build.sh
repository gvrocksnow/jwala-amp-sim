#!/usr/bin/env bash
# Builds the Jwala standalone (Rust) on macOS or Linux and packages it with its app icon into dist/.
# (Windows: use build.ps1.)
#
#   macOS:  dist/Jwala.app             drag to /Applications; Finder and the Dock show Jwala.icns
#   Linux:  dist/jwala                 the standalone (it also sets its own window icon when run)
#           dist/install-linux.sh      installs it, jwala.desktop and the hicolor icons for the
#                                      current user (~/.local), or system-wide with sudo
#
# The app icons in ../assets/appicon/ come from tools/make_app_icons.py.
set -euo pipefail
cd "$(dirname "$0")"
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release --bin jwala

icons=../assets/appicon
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf dist
mkdir -p dist

case "$(uname -s)" in
Darwin)
    app=dist/Jwala.app/Contents
    mkdir -p "$app/MacOS" "$app/Resources"
    cp target/release/jwala "$app/MacOS/jwala"
    cp "$icons/Jwala.icns" "$app/Resources/Jwala.icns"
    sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Info.plist"
    printf 'APPL????' > "$app/PkgInfo"
    # ad-hoc signature, so Apple Silicon Macs run it (Gatekeeper still asks on first launch)
    codesign --force --deep --sign - dist/Jwala.app 2>/dev/null || echo "note: codesign failed; the app may not open on Apple Silicon"
    ;;
Linux)
    cp target/release/jwala dist/jwala
    cp packaging/linux/jwala.desktop dist/
    mkdir -p dist/icons
    for n in 16 24 32 48 64 128 256 512; do cp "$icons/jwala_$n.png" "dist/icons/jwala_$n.png"; done
    cp packaging/linux/install-linux.sh dist/
    chmod +x dist/jwala dist/install-linux.sh
    ;;
*)
    echo "unsupported OS: $(uname -s) (on Windows use build.ps1)" >&2
    exit 1
    ;;
esac
find dist -type f -exec ls -lh {} + | awk '{print $5, $9}'
