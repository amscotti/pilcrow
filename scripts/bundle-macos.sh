#!/usr/bin/env bash
# Wrap the current GPUI binary in a Finder-launchable Pilcrow.app.
#
# Default is the debug profile for local testing. Set PILCROW_PROFILE=release
# for a release artifact, and PILCROW_VERSION to stamp CFBundleShortVersionString.
# Signing is deliberately skipped: this bundle is unsigned by policy.
set -euo pipefail
cd "$(dirname "$0")/.."

profile="${PILCROW_PROFILE:-debug}"
if [ "$profile" = "release" ]; then
    cargo build -p pilcrow-app --release --locked
    binary="target/release/pilcrow"
    app="target/release/Pilcrow.app"
else
    cargo build -p pilcrow-app --locked
    binary="target/debug/pilcrow"
    app="target/Pilcrow.app"
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$binary" "$app/Contents/MacOS/pilcrow"
cp macos/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
cp macos/Info.gpui.plist "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
if [ -n "${PILCROW_VERSION:-}" ]; then
    /usr/libexec/PlistBuddy \
        -c "Set :CFBundleShortVersionString $PILCROW_VERSION" \
        "$app/Contents/Info.plist"
fi

echo "built $app"
echo "open it with: open $app"
