#!/usr/bin/env python3
"""Build the Android JNI worker using the same toolchain as the DB2010 engine."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess

from build_qemu import ROOT, setup_tools


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ndk', type=Path, required=True)
    parser.add_argument('--clang', default=shutil.which('clang-19') or shutil.which('clang'))
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    build = ROOT / 'build/android/qemu'
    build.mkdir(parents=True, exist_ok=True)
    tool, _, _, env = setup_tools(args.ndk.resolve(), build, args.clang)
    target = 'aarch64-linux-android'
    env.update({
        'BINDGEN_EXTRA_CLANG_ARGS': '--sysroot=' + str(args.ndk.resolve() / 'toolchains/llvm/prebuilt/linux-x86_64/sysroot') + ' --target=aarch64-linux-android24',
        'CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER': str(tool / 'cc'),
        'CC_' + target: str(tool / 'cc'), 'CXX_' + target: str(tool / 'cxx'),
        'AR_' + target: str(tool / 'ar'),
        'CMAKE_TOOLCHAIN_FILE_' + target: str(ROOT / 'frontends/android/build-support/cmake/android-arm64-v8a.toolchain.cmake'),
        'CARGO_TARGET_DIR': str(ROOT / 'build/native'),
    })
    cargo = shutil.which('cargo') or str(Path.home() / '.cargo/bin/cargo')
    subprocess.run([cargo, 'build', '--locked', '--release', '--target', target,
                    '-p', 'e830-android-jni', '--jobs', str(args.jobs)], cwd=ROOT, env=env, check=True)
    output = ROOT / 'frontends/android/app/build/generated/e830JniLibs/arm64-v8a'
    output.mkdir(parents=True, exist_ok=True)
    library = output / 'libe830_android_jni.so'
    shutil.copyfile(ROOT / f'build/native/{target}/release/libe830_android_jni.so', library)
    subprocess.run([tool / 'strip', '--strip-unneeded', library], check=True)
    print('Packaged', library)


if __name__ == '__main__':
    main()
