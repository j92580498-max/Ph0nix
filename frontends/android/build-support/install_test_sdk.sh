#!/usr/bin/env bash
# Install the same Linux packages as sdkmanager using the checksums in Google's SDK catalog.
set -euo pipefail
sdk=${1:?Usage: install_test_sdk.sh SDK_DIRECTORY}
mkdir -p "$sdk"
work=$(mktemp -d "$sdk/.install.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

install_archive() (
    archive=$1
    checksum=$2
    destination=$3
    required=$4
    if [[ -f "$sdk/$destination/$required" ]]; then return; fi
    printf 'Installing %s\n' "$destination"
    for base in https://dl.google.com/android/repository https://mirrors.cloud.tencent.com/AndroidSDK https://redirector.gvt1.com/edgedl/android/repository; do
        if curl --fail --location --silent --show-error --connect-timeout 7 --max-time 900 \
            --output "$work/$archive" "$base/$archive" &&
            (cd "$work" && printf '%s  %s\n' "$checksum" "$archive" | sha1sum --check); then
            break
        fi
    done
    (cd "$work" && printf '%s  %s\n' "$checksum" "$archive" | sha1sum --check --status)
    unpacked="$work/${archive%.zip}"
    mkdir -p "$unpacked" "$(dirname "$sdk/$destination")"
    unzip -q "$work/$archive" -d "$unpacked"
    shopt -s nullglob
    dirs=("$unpacked"/*/)
    [[ ${#dirs[@]} -eq 1 && -f "${dirs[0]}$required" ]]
    # Move the complete directory so an interrupted download or extraction
    # cannot look like an installed SDK component on the next run.
    mv -T "${dirs[0]}" "$sdk/$destination"
    rm -- "$work/$archive"
)

install_archive platform-35_r02.zip 0bb560a90a7a2cbd0dd8348224d518b638fe7949 platforms/android-35 android.jar
install_archive build-tools_r35_linux.zip 2cfaa0bbb2336e9ec18ed3ecea84fa2e2af607bc build-tools/35.0.0 aapt2
install_archive android-ndk-r29-linux.zip 87e2bb7e9be5d6a1c6cdf5ec40dd4e0c6d07c30b ndk/29.0.14206865 toolchains/llvm/prebuilt/linux-x86_64/bin/clang
