#!/usr/bin/env python3
"""Build a portable native launcher and its pinned DB2010 engine on Linux/MSYS2."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'src'))
from db2010.development.source import apply_patches


def run(args, cwd=ROOT):
    print('+', ' '.join(map(str, args)), flush=True)
    subprocess.run(list(map(str, args)), cwd=cwd, check=True)


def windows_dlls(executables, destination):
    objdump = shutil.which('objdump')
    if not objdump:
        raise RuntimeError('Run inside MSYS2 MINGW64 with binutils installed.')
    library_dir = Path(objdump).resolve().parent
    pending = list(executables)
    copied = set()
    while pending:
        source = pending.pop()
        info = subprocess.check_output([objdump, '-p', str(source)], text=True)
        for name in re.findall(r'DLL Name:\s*(\S+)', info):
            candidate = library_dir / name
            if name.lower() in copied or not candidate.is_file():
                continue
            copied.add(name.lower())
            for directory in [destination, destination / 'engine']:
                shutil.copyfile(candidate, directory / name)
            pending.append(candidate)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jobs', type=int, default=2)
    parser.add_argument('--engine', type=Path, help='Reuse an already built engine from this source revision.')
    args = parser.parse_args()
    lock_path = ROOT / 'dependencies/qemu/source-lock.json'
    lock = json.loads(lock_path.read_text())
    revision = apply_patches(ROOT, lock)
    windows = sys.platform == 'win32'
    suffix = '.exe' if windows else ''
    system = 'windows' if windows else 'linux'
    directory = ROOT / 'build' / f'desktop-{system}' / 'engine'
    if args.engine:
        engine = args.engine.resolve()
    else:
        directory.mkdir(parents=True, exist_ok=True)
        options = [flag for flag in lock['configure'] if flag != '--enable-plugins']
        options += ['--disable-plugins', '--disable-gio', '--disable-pixman', '--enable-fdt', '--disable-debug-info']
        if not (directory / 'build.ninja').is_file():
            run(['bash', (ROOT / 'qemu/configure').as_posix(), '--python=' + sys.executable, *options], directory)
        run(['ninja', '-j', str(args.jobs), 'qemu-system-arm' + suffix], directory)
        engine = directory / ('qemu-system-arm' + suffix)
    run(['cargo', 'build', '--locked', '--release', '-p', 'featurephone-desktop', '--jobs', str(args.jobs)])
    app = ROOT / 'build/native/release' / ('ph0nix' + suffix)
    arch = platform.machine().lower().replace('amd64', 'x86_64').replace('arm64', 'aarch64')
    destination = ROOT / 'dist' / f'ph0nix-{system}-{arch}'
    (destination / 'engine').mkdir(parents=True, exist_ok=True)
    shutil.copyfile(app, destination / app.name)
    shutil.copyfile(engine, destination / 'engine' / engine.name)
    (destination / 'provenance.json').write_text(json.dumps({
        'engine_source': revision, 'engine_patches': lock.get('patches', []),
        'source_lock_sha256': hashlib.sha256(lock_path.read_bytes()).hexdigest(),
        'engine_sha256': hashlib.sha256(engine.read_bytes()).hexdigest(),
        'reused_engine': bool(args.engine),
        'app_sha256': hashlib.sha256(app.read_bytes()).hexdigest(),
    }, indent=2)+'\n')
    if not windows:
        for file in [destination / app.name, destination / 'engine' / engine.name]:
            file.chmod(0o755)
    else:
        windows_dlls([app, engine], destination)
    shutil.copyfile(ROOT / 'docs/native-apps.md', destination / 'README.md')
    shutil.copyfile(ROOT / 'frontends/android/THIRD_PARTY_NOTICES.md', destination / 'THIRD_PARTY_NOTICES.md')
    licenses = ROOT / 'frontends/android/app/src/main/assets/licenses'
    shutil.copytree(licenses, destination / 'licenses', dirs_exist_ok=True)
    print('Native app:', destination)


if __name__ == '__main__':
    main()
