#!/bin/sh
# Assemble Kashot.app from an already-built `kashot` binary.
#
# This is the single definition of the macOS bundle: CI
# (.github/workflows/build-rust.yml) calls this script, so a local run
# produces the same bundle layout and the same Info.plist that ships in
# the released .dmg. The plist itself lives next to this script in
# Info.plist.in, with @VERSION@ substituted below.
#
#   dist/macos/make-app.sh \
#     --binary kashot-rs/target/release/kashot \
#     --version 0.7.0 \
#     --arch arm64 \
#     --out-dir kashot-rs/dist
#
# Optional:
#   --ffmpeg PATH    static ffmpeg to bundle at Contents/MacOS/ffmpeg
#   --icns PATH      prebuilt .icns (skips iconutil)
#   --iconset PATH   .iconset to compile with iconutil
#                    (default icons/macos_iconset/Kashot.iconset)
#
# ffmpeg and the icon are each skipped with a warning when unavailable.
# That keeps the script usable on a non-macOS box (no iconutil) and for
# quick local builds. CI passes both explicitly, so a missing file there
# is still a hard failure.
set -eu

PROG=$(basename "$0")
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)

BINARY=
VERSION=
ARCH=
FFMPEG=
ICNS=
ICONSET="$REPO_ROOT/icons/macos_iconset/Kashot.iconset"
ICONSET_GIVEN=0
OUT_DIR=dist

die() {
    echo "$PROG: $*" >&2
    exit 1
}

warn() {
    echo "$PROG: warning: $*" >&2
}

# Print this file's leading comment block as the help text, so the usage
# and the documentation above can never drift apart.
usage() {
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --binary)  BINARY=${2:?--binary needs a path};   shift 2 ;;
        --version) VERSION=${2:?--version needs a value}; shift 2 ;;
        --arch)    ARCH=${2:?--arch needs a value};       shift 2 ;;
        --ffmpeg)  FFMPEG=${2:?--ffmpeg needs a path};    shift 2 ;;
        --icns)    ICNS=${2:?--icns needs a path};        shift 2 ;;
        --iconset) ICONSET=${2:?--iconset needs a path}; ICONSET_GIVEN=1; shift 2 ;;
        --out-dir) OUT_DIR=${2:?--out-dir needs a path};  shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

[ -n "$BINARY" ]  || die "--binary is required"
[ -n "$VERSION" ] || die "--version is required"
[ -n "$ARCH" ]    || die "--arch is required"
[ -f "$BINARY" ]  || die "binary not found: $BINARY"

case "$ARCH" in
    arm64|x64) ;;
    *) die "unknown arch: $ARCH (expected arm64 or x64)" ;;
esac

# The version lands in CFBundleVersion and in a sed replacement, so keep it
# to characters that are safe in both.
case "$VERSION" in
    *[!0-9A-Za-z.+-]*) die "version has unexpected characters: $VERSION" ;;
esac

TEMPLATE="$SCRIPT_DIR/Info.plist.in"
[ -f "$TEMPLATE" ] || die "missing plist template: $TEMPLATE"

APP="$OUT_DIR/Kashot.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# Binary inside the bundle is always named `kashot` (matches
# CFBundleExecutable in Info.plist.in).
cp "$BINARY" "$APP/Contents/MacOS/kashot"
chmod +x "$APP/Contents/MacOS/kashot"

# Icon: compile the repo's .iconset with iconutil unless a prebuilt .icns
# was handed in. iconutil ships with macOS only.
if [ -z "$ICNS" ]; then
    if [ ! -d "$ICONSET" ]; then
        if [ "$ICONSET_GIVEN" = 1 ]; then
            die "iconset not found: $ICONSET"
        fi
        warn "no iconset at $ICONSET - bundle will use the generic app icon"
    elif ! command -v iconutil >/dev/null 2>&1; then
        warn "iconutil not available - bundle will use the generic app icon"
    else
        ICNS="$OUT_DIR/Kashot.icns"
        iconutil -c icns -o "$ICNS" "$ICONSET"
    fi
fi
if [ -n "$ICNS" ]; then
    [ -f "$ICNS" ] || die "icns not found: $ICNS"
    cp "$ICNS" "$APP/Contents/Resources/kashot.icns"
fi

# Bundle the static ffmpeg next to the binary so locate_ffmpeg() finds it
# (Contents/MacOS/ffmpeg): powers audio recording + video conversion
# without a system ffmpeg install.
if [ -n "$FFMPEG" ]; then
    [ -f "$FFMPEG" ] || die "ffmpeg not found: $FFMPEG"
    cp "$FFMPEG" "$APP/Contents/MacOS/ffmpeg"
    chmod +x "$APP/Contents/MacOS/ffmpeg"
else
    warn "no --ffmpeg given - recording and video conversion will need a system ffmpeg"
fi

sed "s/@VERSION@/$VERSION/g" "$TEMPLATE" > "$APP/Contents/Info.plist"

echo "$PROG: built $APP (version $VERSION, arch $ARCH)"
