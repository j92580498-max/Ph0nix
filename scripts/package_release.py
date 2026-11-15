#!/usr/bin/env python3
"""Archive a verified native desktop build for a Ph0nix release."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--platform', choices=['linux', 'windows'], required=True)
    parser.add_argument('--arch', choices=['x86_64', 'aarch64'], required=True)
    parser.add_argument('--version', default='1.0')
    parser.add_argument('--output', type=Path, default=ROOT / 'dist/release')
    args = parser.parse_args()
    if not re.fullmatch(r'\d+\.\d+(?:\.\d+)?', args.version):
        parser.error('Version must be a numeric release version.')
    directory = args.directory.resolve()
    suffix = '.exe' if args.platform == 'windows' else ''
    app = directory / ('ph0nix' + suffix)
    engine = directory / 'engine' / ('qemu-system-arm' + suffix)
    provenance = json.loads((directory / 'provenance.json').read_text())
    lock = ROOT / 'dependencies/qemu/source-lock.json'
    for path, key in [(app, 'app_sha256'), (engine, 'engine_sha256')]:
        if sha256(path) != provenance[key]:
            raise RuntimeError(f'Build provenance differs: {path.name}')
        with path.open('rb') as stream:
            magic = stream.read(4)
        if not (magic.startswith(b'MZ') if suffix else magic == b'\x7fELF'):
            raise RuntimeError(f'Wrong executable format: {path.name}')
    if sha256(lock) != provenance['source_lock_sha256']:
        raise RuntimeError('The engine was built from a different source lock.')
    machines = subprocess.check_output([str(engine), '-machine', 'help'], text=True)
    if not re.search(r'^db2010\s', machines, re.MULTILINE):
        raise RuntimeError('The packaged engine has no DB2010 board.')
    manifest = {
        'version': args.version, 'platform': args.platform, 'architecture': args.arch,
        'source_commit': subprocess.check_output(
            ['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip(),
        'engine_machine_query_verified': True,
        'files': {str(path.relative_to(directory)).replace('\\', '/'): sha256(path)
                  for path in sorted(directory.rglob('*')) if path.is_file()},
    }
    data = (json.dumps(manifest, indent=2) + '\n').encode()
    name = f'ph0nix-{args.version}-{args.platform}-{args.arch}'
    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output / (name + ('.zip' if suffix else '.tar.gz'))
    if suffix:
        with zipfile.ZipFile(archive, 'w', compression=zipfile.ZIP_DEFLATED) as target:
            for path in sorted(directory.rglob('*')):
                if path.is_file(): target.write(path, name + '/' + path.relative_to(directory).as_posix())
            target.writestr(name + '/release.json', data)
    else:
        with tarfile.open(archive, 'w:gz') as target:
            target.add(directory, arcname=name)
            info = tarfile.TarInfo(name + '/release.json')
            info.size = len(data); info.mode = 0o644
            target.addfile(info, io.BytesIO(data))
    archive.with_name(archive.name + '.sha256').write_text(sha256(archive) + '  ' + archive.name + '\n')
    print(archive)


if __name__ == '__main__':
    main()
