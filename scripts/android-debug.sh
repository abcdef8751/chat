#!/usr/bin/env bash
# Build a DEBUG Android APK for on-device debugging, pinned to aarch64.
#
# Debug builds keep the default debug signing and isDebuggable = true, and
# still embed the frontend (beforeBuildCommand -> dist/ -> bundled into the
# .so), so the APK is self-contained — no dev server needed. Pinned to the
# aarch64 ABI by default since that's where on-device debugging happens
# (much faster than the full 4-ABI universal debug build).
#
# Usage:
#   npm run android:debug                   # aarch64 debug APK (default)
#   ANDROID_TARGET=armv7 npm run android:debug   # pin another single ABI
#   ANDROID_TARGET=all  npm run android:debug   # all four ABIs (universal)
set -euo pipefail

cd "$(dirname "$0")/.."

# Locate the Android SDK (override with ANDROID_HOME if you have it elsewhere).
export ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
export ANDROID_SDK_ROOT="$ANDROID_HOME"

# Pick the newest installed NDK if ANDROID_NDK_HOME isn't already set.
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  ndk_root="$ANDROID_HOME/ndk"
  if [ -d "$ndk_root" ]; then
    export ANDROID_NDK_HOME="$(ls -1d "$ndk_root"/*/ | sort -V | tail -n 1 | sed 's#/$##')"
  else
    echo "error: no NDK under $ndk_root (set ANDROID_NDK_HOME)" >&2
    exit 1
  fi
fi

# Pin the ABI. Default is aarch64 (the debug device); override with
# ANDROID_TARGET (e.g. armv7, i686, x86_64, or `all` for the universal APK).
TARGET="${ANDROID_TARGET:-aarch64}"

echo "ANDROID_HOME=$ANDROID_HOME"
echo "ANDROID_NDK_HOME=$ANDROID_NDK_HOME"
echo "ANDROID_TARGET=$TARGET (aarch64 is the default debug target)"

if [ "$TARGET" = "all" ]; then
  node_modules/.bin/tauri android build --apk --debug "$@"
else
  node_modules/.bin/tauri android build --apk --debug -t "$TARGET" "$@"
fi
