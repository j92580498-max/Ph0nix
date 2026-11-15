#!/usr/bin/env python3
"""Verify the test APK's signature, manifest, JNI exports and API-24 ELF imports."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[3]


def run(arguments):
    try:
        return subprocess.check_output(list(map(str, arguments)), text=True, stderr=subprocess.STDOUT)
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f'{arguments[0]} failed: {error.output.strip()}') from error


def symbols(readelf, path):
    defined, required = set(), set()
    for line in run([readelf, '--dyn-syms', '--wide', path]).splitlines():
        fields = line.split()
        if len(fields) < 8 or not re.fullmatch(r'\d+:', fields[0]):
            continue
        name = fields[7].split('@', 1)[0]
        if fields[6] != 'UND':
            defined.add(name)
        elif fields[4] == 'GLOBAL':
            required.add(name)
    return defined, required


def signature(apksigner, apk):
    output = run([apksigner, 'verify', '--verbose', '--print-certs', apk])
    if 'Verified using v2 scheme (APK Signature Scheme v2): true' not in output:
        raise RuntimeError('APK Signature Scheme v2 verification failed')
    certificates = re.findall(r'Signer #\d+ certificate SHA-256 digest: (\S+)', output)
    if not certificates:
        raise RuntimeError('Verified APK has no signing certificate')
    return certificates


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('apk', type=Path)
    parser.add_argument('--ndk', required=True, type=Path)
    parser.add_argument('--aapt2', required=True, type=Path)
    parser.add_argument('--apksigner', default='apksigner')
    parser.add_argument('--readelf', default=shutil.which('llvm-readelf') or 'readelf')
    parser.add_argument('--reference-apk', type=Path)
    parser.add_argument('--report', required=True, type=Path)
    args = parser.parse_args()
    gradle = (ROOT / 'frontends/android/app/build.gradle.kts').read_text()
    version = re.search(r'versionName\s*=\s*"([^"]+)"', gradle).group(1)
    version_code = int(re.search(r'versionCode\s*=\s*(\d+)', gradle).group(1))
    badging = run([args.aapt2, 'dump', 'badging', args.apk])
    package = re.search(r"package: name='([^']+)' versionCode='([^']+)' versionName='([^']+)'", badging)
    if not package or package.groups() != ('org.featurephone.emulator', str(version_code), version):
        raise RuntimeError('Packaged app identity/version differs from the source')
    if not re.search(r"(?:minSdkVersion|sdkVersion):'24'", badging) or "targetSdkVersion:'35'" not in badging:
        raise RuntimeError('Packaged Android API requirements differ')
    if "application-label:'ph0nix'" not in badging:
        raise RuntimeError('Packaged application label differs')
    certificates = signature(args.apksigner, args.apk)
    if args.reference_apk and certificates != signature(args.apksigner, args.reference_apk):
        raise RuntimeError('Signing certificate changed: in-place upgrade would fail')

    bridge = (ROOT / 'frontends/android/app/src/main/java/com/samsung/e830/NativeBridge.kt').read_text()
    jni_names = {'Java_com_samsung_e830_NativeBridge_' + name
                 for name in re.findall(r'external fun\s+(\w+)\(', bridge)}
    sysroot = args.ndk / 'toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/24'
    report = {
        'artifact': args.apk.name,
        'version': version,
        'version_code': version_code,
        'apk_bytes': args.apk.stat().st_size,
        'apk_sha256': hashlib.sha256(args.apk.read_bytes()).hexdigest(),
        'min_android_api': 24,
        'target_android_api': 35,
        'signing_certificate_sha256': certificates,
        'signature_v2_verified': True,
        'upgrade_certificate_verified': bool(args.reference_apk),
        'native_libraries': {},
        'android_device_runtime_tested': False,
    }
    with tempfile.TemporaryDirectory(prefix='ph0nix-apk-') as temporary, zipfile.ZipFile(args.apk) as archive:
        libraries = sorted(name for name in archive.namelist() if name.startswith('lib/') and name.endswith('.so'))
        if libraries != ['lib/arm64-v8a/libdb2010_engine.so', 'lib/arm64-v8a/libe830_android_jni.so']:
            raise RuntimeError('Unexpected native libraries or Android ABIs')
        dex = b''.join(archive.read(name) for name in archive.namelist() if re.fullmatch(r'classes\d*\.dex', name))
        for name in ('MainActivity', 'EmulatorActivity', 'DeveloperPanel', 'PhoneFilePanel', 'BluetoothBridge'):
            if f'Lcom/samsung/e830/{name};'.encode() not in dex:
                raise RuntimeError(f'Missing packaged Android class: {name}')
        for member in libraries:
            content = archive.read(member)
            path = Path(temporary) / Path(member).name
            path.write_bytes(content)
            header = run([args.readelf, '--file-header', path])
            if not re.search(r'Machine:\s+AArch64', header) or not re.search(r'Type:\s+DYN', header):
                raise RuntimeError(f'Invalid Android ELF type: {path.name}')
            segments = run([args.readelf, '--program-headers', '--wide', path])
            alignment = [int(line.split()[-1], 16) for line in segments.splitlines() if line.lstrip().startswith('LOAD ')]
            if not alignment or any(value < 16384 for value in alignment):
                raise RuntimeError(f'ELF cannot load on a 16-KiB Android device: {path.name}')
            dynamic = run([args.readelf, '--dynamic', path])
            needed = re.findall(r'\(NEEDED\).*?\[([^]]+)\]', dynamic)
            provided = set()
            for library in needed:
                stub = sysroot / library
                if not stub.is_file():
                    raise RuntimeError(f'Unpackaged Android dependency: {library}')
                provided.update(symbols(args.readelf, stub)[0])
            defined, required = symbols(args.readelf, path)
            missing = sorted(required - provided)
            if missing:
                raise RuntimeError(f'{path.name} requires unavailable API-24 symbols: {missing}')
            if path.name == 'libe830_android_jni.so' and not jni_names.issubset(defined):
                raise RuntimeError(f'Missing JNI exports: {sorted(jni_names - defined)}')
            report['native_libraries'][path.name] = {
                'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest(),
                'load_alignment': alignment, 'needed': needed,
                'api24_required_symbols_verified': True,
            }
        report['jni_methods_verified'] = len(jni_names)
        report['ui_classes_verified'] = 5

    provenance = json.loads((ROOT / 'build/android/qemu/provenance.json').read_text())
    if provenance['engine_sha256'] != report['native_libraries']['libdb2010_engine.so']['sha256']:
        raise RuntimeError('Packaged QEMU engine differs from recorded build provenance')
    report['engine_source'] = provenance['engine_source']
    report['engine_patches'] = provenance['engine_patches']
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + '\n')
    print(f"Verified ph0nix {version}: {len(jni_names)} JNI methods, API 24, ARM64, 16-KiB ELF alignment, signature and engine provenance.")
    print(f'Report: {args.report}')


if __name__ == '__main__':
    main()
