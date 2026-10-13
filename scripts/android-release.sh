#!/usr/bin/env bash
# Build a signed Android release APK.
#
# Pinned to aarch64 by default since that covers essentially all modern
# devices; the other ABIs must be explicitly requested. ABIs still compile
# serially into the shared src-tauri/target/ dir (cargo serializes on that
# lock, so parallel Gradle flags don't help here — see the NOTE in
# gen/android/gradle.properties). CARGO_BUILD_JOBS is exposed but defaults to
# leaving cargo free to use all cores within each ABI build.
#
# Usage:
#   npm run android:release                   # aarch64 release APK (default)
#   ANDROID_TARGET=armv7 npm run android:release   # pin another single ABI
#   ANDROID_TARGET=all  npm run android:release   # all four ABIs (universal)
#   CARGO_BUILD_JOBS=8 npm run android:release   # override the cargo job cap
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

# Default the per-ABI cargo job cap to ~one quarter of the logical CPUs.
: "${CARGO_BUILD_JOBS:=$(( ($(nproc) + 3) / 4 ))}"

# Pin the ABI. Default is aarch64 (arm64, ~all modern devices); override with
# ANDROID_TARGET (e.g. armv7, i686, x86_64, or `all` for the universal APK).
TARGET="${ANDROID_TARGET:-aarch64}"

echo "ANDROID_HOME=$ANDROID_HOME"
echo "ANDROID_NDK_HOME=$ANDROID_NDK_HOME"
echo "ANDROID_TARGET=$TARGET (aarch64 is the default release target)"
echo "CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS"

if [ "$TARGET" = "all" ]; then
  CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" node_modules/.bin/tauri android build --apk "$@"
else
  CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" node_modules/.bin/tauri android build --apk -t "$TARGET" "$@"
fi
