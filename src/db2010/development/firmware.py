"""Fetch or check authenticated K750i firmware and optional provisioning inputs."""
import argparse
from pathlib import Path
import tempfile
import urllib.request

from ..paths import ROOT, RESOURCES
from ..preparation.inputs import digest, load_json, resource
from ..preparation.k750 import container, gdfs_backup, customization_archive


def firmware(root, *, download=False, include_gdfs=False, include_customization=False):
    manifest = load_json(RESOURCES/'k750-r1db001-cid49-manifest.json')
    source = load_json(RESOURCES/'k750-r1db001-cid49-source.json')
    if source['format_version'] != 1 or manifest['format_version'] != 1:
        raise ValueError('Unsupported K750 firmware source format')
    paths = []
    for entry in manifest['entries']:
        path = resource(root, entry['input_path'])
        if path.exists() or not download:
            container(root, entry)
        else:
            url = (f'https://raw.githubusercontent.com/{source["repository"]}/'
                   f'{source["commit"]}/{source["paths"][entry["id"]]}')
            with urllib.request.urlopen(url, timeout=30) as stream:
                data = stream.read(entry['size'] + 1)
            if len(data) != entry['size'] or digest(data) != entry['sha256']:
                raise ValueError(f'K750 {entry["id"]} download differs from the audited container')
            path.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=path.parent, prefix='.download-') as temporary:
                pending = Path(temporary)/path.name
                pending.write_bytes(data)
                container(root, entry, pending)
                pending.replace(path)
        paths.append(path)
    extras = []
    if include_gdfs:
        extras.append(('gdfs_backup', 'GDFS', gdfs_backup))
    if include_customization:
        extras.append(('customization_archive', 'customization archive', customization_archive))
    for key, label, validate in extras:
        source_entry = source[key]
        entry = {key: source_entry[key] for key in ('input_path', 'size_bytes', 'sha256')}
        path = resource(root, entry['input_path'])
        if path.exists() or not download:
            validate(root, entry)
        else:
            with urllib.request.urlopen(source_entry['url'], timeout=30) as stream:
                data = stream.read(entry['size_bytes'] + 1)
            if len(data) != entry['size_bytes'] or digest(data) != entry['sha256']:
                raise ValueError(f'K750 {label} download differs from the audited input')
            path.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=path.parent, prefix='.download-') as temporary:
                pending = Path(temporary)/path.name
                pending.write_bytes(data)
                validate(root, entry, pending)
                pending.replace(path)
        paths.append(path)
    return paths


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('init', 'check'))
    parser.add_argument('model', choices=('k750i',))
    parser.add_argument('--repo', type=Path, default=ROOT, help='emulator checkout')
    parser.add_argument('--input-root', type=Path, help='root for downloaded firmware references')
    parser.add_argument('--prefix', action='store_true', help='check or fetch only MAIN/FS for --k750i-prefix')
    args = parser.parse_args()
    root = (args.input_root or args.repo).resolve()
    for path in firmware(root, download=args.action == 'init', include_gdfs=not args.prefix,
                         include_customization=not args.prefix):
        print(f'K750 input verified: {path}')


if __name__ == '__main__':
    main()
