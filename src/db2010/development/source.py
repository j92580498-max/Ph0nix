"""Initialize and check the commit-pinned QEMU submodule using ordinary Git."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

from ..paths import ROOT, DEPENDENCIES, qemu_source
LOCK = DEPENDENCIES / 'source-lock.json'


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


def tag_commit(source, tag):
    try:
        return git(source, 'rev-parse', '--verify', '--quiet', f'refs/tags/{tag}^{{commit}}')
    except subprocess.CalledProcessError as error:
        if error.returncode != 1:
            raise
        return None


def patch_records(root, lock):
    directory = Path(root)/'dependencies/qemu'
    records = []
    for patch in lock.get('patches', []):
        path = (directory/patch['path']).resolve()
        if not path.is_relative_to(directory.resolve()):
            raise RuntimeError('QEMU patch path escapes the dependency directory')
        if hashlib.sha256(path.read_bytes()).hexdigest() != patch['sha256']:
            raise RuntimeError('Pinned QEMU patch identity differs: '+patch['path'])
        if not patch.get('files'):
            raise RuntimeError('Pinned QEMU patch has no source identities')
        for name in patch['files']:
            if not (qemu_source(root)/name).resolve().is_relative_to(qemu_source(root).resolve()):
                raise RuntimeError('QEMU patch source path escapes the checkout')
        records.append((path, patch))
    return records


def verify_patches(root, lock):
    source = qemu_source(root)
    records = patch_records(root, lock)
    # A later patch can extend a file changed by an earlier patch. Each record
    # authenticates that patch's output; the last record is the final identity.
    expected_files = {name: expected for _, patch in records
                      for name, expected in patch['files'].items()}
    allowed = set(expected_files)
    changed = set(git(source, 'diff', '--name-only', 'HEAD').splitlines())
    untracked = set(git(source, 'ls-files', '--others', '--exclude-standard').splitlines())
    if not changed <= allowed or not untracked <= allowed:
        raise RuntimeError('QEMU has uncommitted changes outside the pinned patch series')
    for name, expected in expected_files.items():
        path = source/name
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise RuntimeError('Pinned QEMU patched source identity differs: '+name)


def apply_patches(root, lock):
    """Apply exact published patch bytes; preserve unrelated local changes."""
    source = qemu_source(root)
    if git(source, 'rev-parse', 'HEAD') != lock['fork_commit']:
        raise RuntimeError('QEMU revision differs from source-lock.json')
    records = patch_records(root, lock)
    expected_files = {name: expected for _, patch in records
                      for name, expected in patch['files'].items()}
    allowed = set(expected_files)
    changed = set(git(source, 'diff', '--name-only', 'HEAD').splitlines())
    untracked = set(git(source, 'ls-files', '--others', '--exclude-standard').splitlines())
    if not changed <= allowed or not untracked <= allowed:
        raise RuntimeError('QEMU has uncommitted changes outside the pinned patch series')
    if records and all((source/name).is_file() and
                       hashlib.sha256((source/name).read_bytes()).hexdigest() == expected
                       for name, expected in expected_files.items()):
        verify_patches(root, lock)
        return lock['fork_commit']
    # A partially matching file may contain someone's own edits outside the
    # patch's context. Refuse before applying anything to a modified checkout.
    if changed or untracked:
        raise RuntimeError('Pinned QEMU patch cannot be applied without replacing local changes')
    for path, patch in records:
        if all((source/name).is_file() and hashlib.sha256((source/name).read_bytes()).hexdigest() == expected
               for name, expected in patch['files'].items()):
            continue
        result = subprocess.run(['git', '-C', str(source), 'apply', '--check', str(path)], capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError('Pinned QEMU patch cannot be applied without replacing local changes: '+patch['path'])
        subprocess.run(['git', '-C', str(source), 'apply', str(path)], check=True)
    if records:
        verify_patches(root, lock)
    elif git(source, 'status', '--porcelain'):
        raise RuntimeError('QEMU has uncommitted changes; commit and pin them before building')
    return lock['fork_commit']


def init(root, lock):
    subprocess.run(['git', '-C', str(root), 'submodule', 'update', '--init', 'qemu'], check=True)
    source = qemu_source(root)
    if tag_commit(source, lock['tag']) is None:
        subprocess.run(['git', '-C', str(source), 'fetch', '--no-tags', lock['upstream'],
                        'tag', lock['tag']], check=True)
    apply_patches(root, lock)
    return check(root, lock)


def check(root, lock):
    source = qemu_source(root)
    if not (source/'.git').exists():
        raise RuntimeError('QEMU submodule is not initialized; run python -m db2010.development.source init')
    if lock.get('patches'):
        verify_patches(root, lock)
    elif git(source, 'status', '--porcelain'):
        raise RuntimeError('QEMU has uncommitted changes; commit and pin them before building')
    if git(source, 'rev-parse', 'HEAD') != lock['fork_commit']:
        raise RuntimeError('QEMU revision differs from source-lock.json; update the submodule pin and lock together')
    base = tag_commit(source, lock['tag'])
    if base is None:
        raise RuntimeError(f"QEMU release tag {lock['tag']} is missing; "
                           'run python -m db2010.development.source init to fetch it')
    if base != lock['base_commit']:
        raise RuntimeError('QEMU release tag differs from the pinned base commit')
    git(source, 'merge-base', '--is-ancestor', lock['base_commit'], 'HEAD')
    pin = git(root, 'ls-files', '--stage', 'qemu').split()
    if len(pin) < 2 or pin[:2] != ['160000', lock['fork_commit']]:
        raise RuntimeError('QEMU submodule index pin differs from source-lock.json')
    return lock['fork_commit']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['init', 'check'])
    parser.add_argument('--repo', type=Path, default=ROOT, help='emulator checkout')
    args = parser.parse_args()
    root = args.repo.resolve()
    lock = json.loads((root/'dependencies/qemu/source-lock.json').read_text())
    if lock['format_version'] != 1:
        raise ValueError('Unsupported source lock')
    verify = init if args.action == 'init' else check
    print(f'QEMU pin verified: {verify(root, lock)}')


if __name__ == '__main__':
    main()
