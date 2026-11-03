#!/bin/sh
# Builds vendor/macos/libwooting-rgb-sdk.dylib for Apple Silicon from the Wooting RGB SDK v1.8.0
# source tag, with its hidapi submodule (0.14.0) compiled in: the result links only system
# frameworks. Needs git and the Xcode command line tools.
#
# Usage: scripts/build-wooting-sdk-macos.sh [work dir]
set -eu

TAG=v1.8.0
COMMIT=98364bcd85aa267ee1cd657197f5d886175251f1
HIDAPI_COMMIT=d3013f0af3f4029d82872c1a9487ea461a56dee4
ARCH=arm64
MIN_MACOS=14.4

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-$(mktemp -d)}
SRC="$WORK/wooting-rgb-sdk"
OUT="$ROOT/vendor/macos/libwooting-rgb-sdk.dylib"

if [ ! -d "$SRC/.git" ] && [ ! -f "$SRC/.git" ]; then
    git clone --quiet --branch "$TAG" --depth 1 --recurse-submodules --shallow-submodules \
        https://github.com/WootingKb/wooting-rgb-sdk.git "$SRC"
fi
[ "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT" ] || { echo "unexpected SDK commit" >&2; exit 1; }
[ "$(git -C "$SRC/hidapi" rev-parse HEAD)" = "$HIDAPI_COMMIT" ] || { echo "unexpected hidapi commit" >&2; exit 1; }

# hidapi 0.14 seizes every device it opens on macOS (kIOHIDOptionsTypeSeizeDevice) "for backward
# compatibility". Open shared instead, as the SDK does on Windows and Linux, so Wootility and other
# apps can still reach the keyboard while Wootlight runs. This is the only change to the sources.
HID_C="$WORK/hid.c"
sed 's|hid_darwin_set_open_exclusive(1); /\* Backward compatibility \*/|hid_darwin_set_open_exclusive(0);|' \
    "$SRC/hidapi/mac/hid.c" > "$HID_C"
grep -q 'hid_darwin_set_open_exclusive(0);' "$HID_C" || { echo "hidapi patch did not apply" >&2; exit 1; }

CFLAGS="-arch $ARCH -mmacosx-version-min=$MIN_MACOS -O2 -fPIC -Wall -Wno-unused-function"
OBJ="$WORK/obj"
mkdir -p "$OBJ"
# shellcheck disable=SC2086
clang $CFLAGS -I"$SRC/hidapi/hidapi" -I"$SRC/hidapi/mac" -c "$HID_C" -o "$OBJ/hid.o"
for f in wooting-rgb-sdk wooting-usb; do
    # shellcheck disable=SC2086
    clang $CFLAGS -I"$SRC/hidapi/hidapi" -I"$SRC/src" -c "$SRC/src/$f.c" -o "$OBJ/$f.o"
done

# Export the SDK API only; hidapi stays internal. Wootlight loads the library by path.
mkdir -p "$(dirname "$OUT")"
clang -arch "$ARCH" -mmacosx-version-min="$MIN_MACOS" -dynamiclib \
    -install_name @rpath/libwooting-rgb-sdk.dylib \
    -compatibility_version 1.8.0 -current_version 1.8.0 \
    -Wl,-exported_symbol,'_wooting_*' -Wl,-dead_strip -Wl,-x \
    "$OBJ/wooting-rgb-sdk.o" "$OBJ/wooting-usb.o" "$OBJ/hid.o" \
    -framework IOKit -framework CoreFoundation -framework AppKit \
    -o "$OUT"
codesign --force --sign - "$OUT"

echo "built $OUT"
lipo -info "$OUT"
otool -L "$OUT"
