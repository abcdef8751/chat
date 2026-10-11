#!/usr/bin/env bash
# Build a signed Android release APK.
#
# This compiles all four ABIs serially into the shared src-tauri/target/ dir
# (cargo serializes on that lock, so parallel Gradle flags don't help here —
# see the NOTE in gen/android/gradle.properties). CARGO_BUILD_JOBS is exposed
# but defaults to leaving cargo free to use all cores within each ABI build.
#
# Usage:
#   npm run android:release            # build the universal fat APK (all 4 ABIs)
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

echo "ANDROID_HOME=$ANDROID_HOME"
echo "ANDROID_NDK_HOME=$ANDROID_NDK_HOME"
echo "CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS"

CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" node_modules/.bin/tauri android build --apk "$@"
