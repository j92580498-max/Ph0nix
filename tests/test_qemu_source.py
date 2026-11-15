"""Source checks stay offline; initialization obtains missing upstream tags."""
import hashlib
import subprocess

import pytest

from db2010.development.source import apply_patches, check, init


def git(path, *args):
    return subprocess.check_output(['git', '-C', str(path), *args], text=True).strip()


@pytest.fixture
def pinned_source(tmp_path):
    root = tmp_path/'checkout'; root.mkdir()
    git(root, 'init', '-q', '-b', 'main')
    git(root, 'config', 'user.name', 'Fixture')
    git(root, 'config', 'user.email', 'fixture@example.invalid')
    qemu = root/'qemu'; qemu.mkdir()
    git(qemu, 'init', '-q', '-b', 'db2010')
    git(qemu, 'config', 'user.name', 'Fixture')
    git(qemu, 'config', 'user.email', 'fixture@example.invalid')
    version = qemu/'VERSION'; version.write_text('base\n')
    git(qemu, 'add', 'VERSION'); git(qemu, 'commit', '-qm', 'base')
    git(qemu, 'tag', 'v-test')
    base = git(qemu, 'rev-parse', 'HEAD')
    version.write_text('fork\n'); git(qemu, 'commit', '-qam', 'fork')
    head = git(qemu, 'rev-parse', 'HEAD')
    lock = {'tag': 'v-test', 'base_commit': base, 'fork_commit': head}
    git(root, 'update-index', '--add', '--cacheinfo', f'160000,{head},qemu')
    return root, lock


def test_submodule_pin_and_dirty_revision_guards(pinned_source):
    root, lock = pinned_source
    qemu = root/'qemu'; version = qemu/'VERSION'
    assert check(root, lock) == lock['fork_commit']
    version.write_text('dirty\n')
    with pytest.raises(RuntimeError, match='uncommitted changes'):
        check(root, lock)
    version.write_text('fork\n')
    with pytest.raises(RuntimeError, match='revision differs'):
        check(root, {**lock, 'fork_commit': lock['base_commit']})
    with pytest.raises(RuntimeError, match='release tag'):
        check(root, {**lock, 'base_commit': '0'*40})
    git(qemu, 'tag', '-d', lock['tag'])
    with pytest.raises(RuntimeError, match='release tag .* missing; run python -m db2010.development.source init'):
        check(root, lock)
    git(qemu, 'tag', lock['tag'], lock['base_commit'])
    git(root, 'update-index', '--cacheinfo', f"160000,{lock['base_commit']},qemu")
    with pytest.raises(RuntimeError, match='index pin'):
        check(root, lock)
    with pytest.raises(RuntimeError, match='not initialized'):
        check(root/'missing', lock)


def test_init_fetches_missing_tag_from_upstream(pinned_source, tmp_path, monkeypatch):
    root, lock = pinned_source
    qemu = root/'qemu'
    upstream = tmp_path/'upstream.git'
    # Only the upstream has the annotated release tag; the fork has the commits.
    git(qemu, 'tag', '-f', '-a', '-m', 'release', lock['tag'], lock['base_commit'])
    git(tmp_path, 'clone', '--bare', str(qemu), str(upstream))
    git(qemu, 'tag', '-d', lock['tag'])
    git(root, 'config', '-f', '.gitmodules', 'submodule.qemu.path', 'qemu')
    git(root, 'config', '-f', '.gitmodules', 'submodule.qemu.url', str(qemu))
    git(root, 'add', '.gitmodules')
    git(root, 'commit', '-qm', 'pin fork')
    fresh = tmp_path/'fresh'
    git(tmp_path, 'clone', str(root), str(fresh))
    monkeypatch.setenv('GIT_ALLOW_PROTOCOL', 'file')
    lock = {**lock, 'upstream': str(upstream)}

    assert init(fresh, lock) == lock['fork_commit']
    assert check(fresh, lock) == lock['fork_commit']
    assert git(fresh/'qemu', 'cat-file', '-t', f"refs/tags/{lock['tag']}") == 'tag'
    assert git(fresh/'qemu', 'rev-parse', 'HEAD') == lock['fork_commit']

    # Existing tags need no upstream connection, and conflicts are not replaced.
    offline_lock = {**lock, 'upstream': str(tmp_path/'unavailable.git')}
    assert init(fresh, offline_lock) == lock['fork_commit']
    git(fresh/'qemu', 'tag', '-f', lock['tag'], lock['fork_commit'])
    with pytest.raises(RuntimeError, match='release tag differs'):
        init(fresh, offline_lock)
    assert git(fresh/'qemu', 'rev-parse', f"refs/tags/{lock['tag']}") == lock['fork_commit']


@pytest.fixture
def patched_source(pinned_source):
    root, lock = pinned_source
    source = root/'qemu'
    version = source/'VERSION'
    version.write_text('ph0nix compatibility\n')
    directory = root/'dependencies/qemu/patches'
    directory.mkdir(parents=True)
    patch = directory/'compatibility.patch'
    patch.write_text(git(source, 'diff', 'HEAD')+'\n')
    lock = {**lock, 'patches': [{
        'path': 'patches/compatibility.patch',
        'sha256': hashlib.sha256(patch.read_bytes()).hexdigest(),
        'files': {'VERSION': hashlib.sha256(version.read_bytes()).hexdigest()},
    }]}
    version.write_text('fork\n')
    return root, lock


def test_published_patch_applies_offline_and_is_idempotent(patched_source):
    root, lock = patched_source
    assert apply_patches(root, lock) == lock['fork_commit']
    assert (root/'qemu/VERSION').read_text() == 'ph0nix compatibility\n'
    assert check(root, lock) == lock['fork_commit']
    assert apply_patches(root, lock) == lock['fork_commit']
    assert git(root/'qemu', 'rev-parse', 'HEAD') == lock['fork_commit']


def test_patch_refuses_local_edits_before_mutating_any_file(patched_source):
    root, lock = patched_source
    source = root/'qemu'
    version = source/'VERSION'
    version.write_text('fork\nlocal notes outside the patch\n')
    before = version.read_bytes()
    with pytest.raises(RuntimeError, match='without replacing local changes'):
        apply_patches(root, lock)
    assert version.read_bytes() == before
    version.write_text('fork\n')
    (source/'local-file').write_text('preserve this too\n')
    with pytest.raises(RuntimeError, match='outside the pinned patch series'):
        apply_patches(root, lock)
    assert version.read_text() == 'fork\n'
    assert (source/'local-file').read_text() == 'preserve this too\n'


def test_patched_source_and_patch_bytes_are_authenticated(patched_source):
    root, lock = patched_source
    apply_patches(root, lock)
    version = root/'qemu/VERSION'
    version.write_text(version.read_text()+'local notes\n')
    with pytest.raises(RuntimeError, match='patched source identity differs'):
        check(root, lock)
    patch = root/'dependencies/qemu/patches/compatibility.patch'
    patch.write_bytes(patch.read_bytes()+b'\n')
    with pytest.raises(RuntimeError, match='patch identity differs'):
        apply_patches(root, lock)


@pytest.fixture
def overlapping_source(pinned_source):
    root, lock = pinned_source
    source = root/'qemu'
    directory = root/'dependencies/qemu/patches'
    directory.mkdir(parents=True)
    version = source/'VERSION'
    version.write_text('first patch\n')
    first = directory/'first.patch'
    first.write_text(git(source, 'diff', 'HEAD')+'\n')
    records = [{
        'path': 'patches/first.patch',
        'sha256': hashlib.sha256(first.read_bytes()).hexdigest(),
        'files': {'VERSION': hashlib.sha256(version.read_bytes()).hexdigest()},
    }]
    # Temporary fixture commits provide the second patch's input tree. The
    # production source revision stays pinned when applying the published series.
    git(source, 'add', 'VERSION')
    git(source, 'commit', '-qm', 'temporary first patch tree')
    version.write_text('second patch\n')
    added = source/'codec.c'
    added.write_text('int portable_codec(void) { return 0; }\n')
    git(source, 'add', 'codec.c')
    second = directory/'second.patch'
    second.write_text(git(source, 'diff', 'HEAD')+'\n')
    records.append({
        'path': 'patches/second.patch',
        'sha256': hashlib.sha256(second.read_bytes()).hexdigest(),
        'files': {name: hashlib.sha256((source/name).read_bytes()).hexdigest()
                  for name in ['VERSION', 'codec.c']},
    })
    git(source, 'reset', '--hard', lock['fork_commit'])
    return root, {**lock, 'patches': records}


def test_overlapping_patches_and_added_sources_apply_offline(overlapping_source):
    root, lock = overlapping_source
    assert apply_patches(root, lock) == lock['fork_commit']
    assert (root/'qemu/VERSION').read_text() == 'second patch\n'
    assert (root/'qemu/codec.c').read_text() == 'int portable_codec(void) { return 0; }\n'
    assert check(root, lock) == lock['fork_commit']
    assert apply_patches(root, lock) == lock['fork_commit']
    assert git(root/'qemu', 'rev-parse', 'HEAD') == lock['fork_commit']


def test_pinned_added_sources_never_overwrite_local_work(overlapping_source):
    root, lock = overlapping_source
    added = root/'qemu/codec.c'
    added.write_text('local decoder work\n')
    with pytest.raises(RuntimeError, match='without replacing local changes'):
        apply_patches(root, lock)
    assert added.read_text() == 'local decoder work\n'
    assert (root/'qemu/VERSION').read_text() == 'fork\n'
    added.unlink()
    apply_patches(root, lock)
    added.write_text('modified decoder\n')
    with pytest.raises(RuntimeError, match='patched source identity differs'):
        check(root, lock)
    with pytest.raises(RuntimeError, match='without replacing local changes'):
        apply_patches(root, lock)
    assert added.read_text() == 'modified decoder\n'
