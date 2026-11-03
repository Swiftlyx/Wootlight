#!/bin/sh
# Builds target/Wootlight.app for Apple Silicon: release build, Info.plist, icon, ad-hoc signature.
# The Wooting SDK stays embedded in the executable and is extracted on start, as on Windows.
#
# Usage: scripts/bundle-macos.sh
set -eu

TARGET=aarch64-apple-darwin
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$VERSION" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

command -v cargo >/dev/null || . "$HOME/.cargo/env"
cargo build --release --target "$TARGET"

APP="$ROOT/target/Wootlight.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/Wootlight" "$APP/Contents/MacOS/Wootlight"
sed "s/@VERSION@/$VERSION/g" macos/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Icon set from the 512 px artwork; the 1024 px slot is left out rather than upscaled.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
ICONSET="$WORK/Wootlight.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/Wootlight-512.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if [ "$double" -le 512 ]; then
        sips -z "$double" "$double" assets/Wootlight-512.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Wootlight.icns"

codesign -s - --force --deep "$APP"
codesign --verify --strict "$APP"
echo "built $APP ($VERSION)"
