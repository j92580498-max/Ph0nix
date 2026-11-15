"""Sparse K750 containers retain byte identity and guest-written RAM execution."""
import io
import copy
import json
from pathlib import Path
import shutil
import struct
import zipfile

import pytest

from db2010.development import firmware
from db2010.diagnostics.artifacts import artifact
from db2010.preparation import k750
from db2010.preparation.inputs import digest, prepare
from support.fixtures import qemu_run, words

HANDLER, SOURCE, RAM = 0x44021000, 0x44022000, 0x4C000000


def image(root, identity, records):
    data = bytearray(0x2EC)
    data[:4] = bytes.fromhex('babe0003')
    struct.pack_into('<I', data, 0x2E8, len(records))
    segments, ranges = [], []
    for address, raw in records:
        segments.append({'offset': len(data)+8, 'address': address, 'length': len(raw)})
        if ranges and address == ranges[-1][1]:
            ranges[-1][1] += len(raw)
        else:
            ranges.append([address, address+len(raw)])
        data.extend(struct.pack('<II', address, len(raw)))
        data.extend(raw)
    end = len(data)
    data.extend(b'opaque authenticated trailer')
    path = root/f'fw/{identity}.babe'
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return {'id': identity, 'input_path': path.relative_to(root).as_posix(),
            'size': len(data), 'sha256': digest(data), 'base': ranges[0][0],
            'end': ranges[-1][1], 'segments': segments, 'ranges': ranges,
            'record_stream_end': end, 'payload_sha256': digest(b''.join(raw for _, raw in records))}


def inputs(tmp_path, monkeypatch, *, corrupt_copy=False, second_copy=False):
    resources = tmp_path/'resources'
    resources.mkdir()
    vector = bytearray(0x24)
    struct.pack_into('<I', vector, 0, 0xE59FF018)
    struct.pack_into('<I', vector, 0x20, HANDLER)
    copies = [(RAM, SOURCE, words(0xE3A0302A, 0xEAFFFFFE))]
    if second_copy:
        copies.append((RAM+0x100, SOURCE+0x1000, words(0xE3A0302B, 0xEAFFFFFE)))
    program, literal_loads, literals = [], [], []
    for i, (address, source, original) in enumerate(copies):
        literal_loads.extend([(len(program), 0), (len(program)+1, 1)])
        literals.extend([address, source])
        program.extend([0, 0, 0xE5912000,
            *([0xE2822001] if corrupt_copy and i == len(copies)-1 else []),
            0xE5802000, 0xE5912004, 0xE5802004])
    program.append(0xE12FFF10)
    for i, (index, register) in enumerate(literal_loads):
        offset = 4*(len(program)+i-index)-8
        program[index] = 0xE59F0000 | (register << 12) | offset
    records = [(source+offset, original[offset:offset+4])
               for _, source, original in copies for offset in (0, 4)]
    main = image(tmp_path, 'main', [(0x44020000, vector),
        (HANDLER, words(*program, *literals)), *records])
    fs = image(tmp_path, 'fs', [(0x45400000, b'FS00')])
    manifest = {'format_version': 1, 'entries': [main, fs]}
    (resources/'k750-r1db001-cid49-manifest.json').write_text(json.dumps(manifest))
    (resources/'k750-r1db001-cid49-source.json').write_text(json.dumps({
        'format_version': 1, 'repository': 'fixture/firmware', 'commit': 'a'*40,
        'paths': {identity: identity+'.babe' for identity in ('main', 'fs')}}))
    contract = {'format_version': 1, 'payload_sha256': main['payload_sha256'],
        'mappings': [{'address': address, 'source': source, 'size': len(original),
                      'sha256': digest(original)} for address, source, original in copies]}
    (resources/'k750-r1db001-cid49-execution-contract.json').write_text(json.dumps(contract))
    baseline = {'cpsr': 0xD3, 'memory': [{'name': 'external_ram', 'address': RAM, 'size': 0x800000}],
        'ram_initialization': {'region': 'external_ram', 'fill_byte': 0},
        'executable_ranges': main['ranges']+[[address, address+len(original)]
                                           for address, _, original in copies],
        'execution_contract': 'k750-r1db001-cid49-execution-contract.json'}
    (resources/'k750-r1db001-cid49-ram-baseline.json').write_text(json.dumps(baseline))
    spec = {'format_version': 1, 'machine': 'db2010', 'experiment': 'controlled-k750',
        'baseline': 'k750-r1db001-cid49-ram-baseline.json', 'instruction_limit': len(program)+2,
        'seconds': 5, 'assumptions': [], 'timebase': {'profile': 'qemu-virtual-v1', 'icount_shift': 6}}
    spec_path = tmp_path/'profile.json'
    spec_path.write_text(json.dumps(spec))
    monkeypatch.setattr(k750, 'RESOURCES', resources)
    monkeypatch.setattr(firmware, 'RESOURCES', resources)
    return manifest, spec_path


def test_sparse_overrides_authenticate_trailer_and_never_seed_ram(tmp_path, monkeypatch):
    manifest, spec = inputs(tmp_path, monkeypatch)
    selected = {}
    for entry in manifest['entries']:
        path = tmp_path/(entry['id']+'.renamed')
        shutil.move(tmp_path/entry['input_path'], path)
        selected[entry['id']] = path
    output = tmp_path/'prepared'
    state, _ = prepare(tmp_path, spec, output, images=selected)
    loaded = json.loads(state.read_text())
    assert loaded['entry'] == HANDLER
    assert loaded['regions'][1]['seeds'] == []
    assert Path(loaded['execution_proofs'][0]['file']).read_bytes() == words(0xE3A0302A, 0xEAFFFFFE)
    seeds = loaded['regions'][0]['seeds']
    assert [r['address'] for r in seeds] == [0x44020000, HANDLER, SOURCE, 0x45400000]
    assert Path(seeds[2]['file']).stat().st_size == 8
    identities = json.loads(artifact(output, 'inputs.json').read_text())['cli_image_overrides']
    assert set(identities) == {'main', 'fs'}
    assert identities['main']['input_path'] == str(selected['main'])
    raw = selected['main'].read_bytes()
    selected['main'].write_bytes(raw[:-1]+bytes([raw[-1]^1]))
    with pytest.raises(ValueError, match='container identity differs'):
        prepare(tmp_path, spec, tmp_path/'changed', images=selected)


@pytest.mark.parametrize('name', ['gdfs', 'nand-main', 'nand-spare'])
def test_k750_rejects_storage_substitution_before_reading(tmp_path, monkeypatch, name):
    _, spec = inputs(tmp_path, monkeypatch)
    with pytest.raises(ValueError, match='GDFS, NAND and synthetic ROM inputs are unsupported'):
        prepare(tmp_path, spec, tmp_path/'prepared', images={name: tmp_path/'missing.bin'})


@pytest.mark.parametrize('field', [
    'prepared_spare', 'synthetic_spare', 'nand_main_image', 'nand_spare_image',
    'block_erase', 'main_array', 'copy_back'])
def test_k750_rejects_uncataloged_nand_storage_in_profile(tmp_path, monkeypatch, field):
    _, spec = inputs(tmp_path, monkeypatch)
    config = json.loads(spec.read_text())
    config['nand'] = {**k750.NAND_PROBE, field: 'uncataloged input'}
    spec.write_text(json.dumps(config))
    with pytest.raises(ValueError, match='unseeded reset/ID/status only'):
        prepare(tmp_path, spec, tmp_path/'prepared')


@pytest.mark.parametrize('second_copy', [False, True])
@pytest.mark.parametrize('corrupt_copy', [False, True])
def test_native_k750_copy_requires_original_guest_written_instructions(tmp_path, monkeypatch, corrupt_copy, second_copy):
    _, spec = inputs(tmp_path, monkeypatch, corrupt_copy=corrupt_copy, second_copy=second_copy)
    state, _ = prepare(tmp_path, spec, tmp_path/'prepared', recording=False)
    loaded = json.loads(state.read_text())
    for region in loaded['regions']:
        for seed in region['seeds']:
            seed['data'] = Path(seed.pop('file')).read_bytes()
    result = qemu_run(loaded, tmp_path/'native', trace=False)
    assert result['stop']['reason'] == ('fetch_original_copy_mismatch' if corrupt_copy else 'instruction_limit')
    assert result['registers'][3] == (0 if corrupt_copy else 43 if second_copy else 42)
    if corrupt_copy:
        assert result['stop']['address'] == RAM+(0x100 if second_copy else 0)


def test_two_copy_contract_preserves_gap_and_empty_ram(tmp_path, monkeypatch):
    _, spec = inputs(tmp_path, monkeypatch, second_copy=True)
    output = tmp_path/'prepared'
    state, _ = prepare(tmp_path, spec, output)
    loaded = json.loads(state.read_text())
    assert loaded['regions'][1]['seeds'] == []
    assert loaded['executable_ranges'][-2:] == [
        {'start': RAM, 'end': RAM+8}, {'start': RAM+0x100, 'end': RAM+0x108}]
    assert [(proof['address'], Path(proof['file']).read_bytes())
            for proof in loaded['execution_proofs']] == [
        (RAM, words(0xE3A0302A, 0xEAFFFFFE)),
        (RAM+0x100, words(0xE3A0302B, 0xEAFFFFFE))]


@pytest.mark.parametrize('change', ['source_gap', 'source_end', 'mapping_hash', 'aperture_gap'])
def test_second_copy_contract_rejects_changed_source_or_aperture(tmp_path, monkeypatch, change):
    _, spec = inputs(tmp_path, monkeypatch, second_copy=True)
    path = k750.RESOURCES/('k750-r1db001-cid49-ram-baseline.json'
        if change == 'aperture_gap' else 'k750-r1db001-cid49-execution-contract.json')
    config = json.loads(path.read_text())
    if change == 'aperture_gap':
        config['executable_ranges'][-2][1] += 4
    elif change == 'mapping_hash':
        config['mappings'][1]['sha256'] = '0'*64
    else:
        config['mappings'][1]['source'] += -4 if change == 'source_gap' else 4
    path.write_text(json.dumps(config))
    with pytest.raises(ValueError, match='original-copy mapping|execution aperture'):
        prepare(tmp_path, spec, tmp_path/'prepared')


def test_firmware_init_is_pinned_authenticated_and_then_offline(tmp_path, monkeypatch):
    manifest, _ = inputs(tmp_path, monkeypatch)
    raw = {entry['id']: (tmp_path/entry['input_path']).read_bytes() for entry in manifest['entries']}
    for entry in manifest['entries']:
        (tmp_path/entry['input_path']).unlink()
    calls = []
    def download(url, **options):
        calls.append(url)
        assert '/'+('a'*40)+'/' in url
        return io.BytesIO(raw[Path(url).stem])
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', download)
    paths = firmware.firmware(tmp_path, download=True)
    assert len(calls) == 2
    assert firmware.firmware(tmp_path, download=True) == paths
    assert firmware.firmware(tmp_path) == paths
    assert len(calls) == 2
    paths[0].write_bytes(b'damaged existing image')
    with pytest.raises(ValueError, match='container identity differs'):
        firmware.firmware(tmp_path, download=True)
    assert paths[0].read_bytes() == b'damaged existing image' and len(calls) == 2


def test_wrong_download_never_publishes_a_firmware_file(tmp_path, monkeypatch):
    manifest, _ = inputs(tmp_path, monkeypatch)
    for entry in manifest['entries']:
        (tmp_path/entry['input_path']).unlink()
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', lambda *a, **k: io.BytesIO(b'wrong firmware'))
    with pytest.raises(ValueError, match='download differs'):
        firmware.firmware(tmp_path, download=True)
    assert not any((tmp_path/entry['input_path']).exists() for entry in manifest['entries'])
    assert not list(tmp_path.rglob('.download-*'))


def test_download_mapping_failure_cleans_pending_file(tmp_path, monkeypatch):
    manifest, _ = inputs(tmp_path, monkeypatch)
    entry = manifest['entries'][0]
    path = tmp_path/entry['input_path']
    raw = path.read_bytes()
    path.unlink()
    entry['ranges'][0][1] += 4
    (firmware.RESOURCES/'k750-r1db001-cid49-manifest.json').write_text(json.dumps(manifest))
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', lambda *a, **k: io.BytesIO(raw))
    with pytest.raises(ValueError, match='payload coverage differs'):
        firmware.firmware(tmp_path, download=True)
    assert not path.exists() and not list(tmp_path.rglob('.download-*'))


def test_missing_firmware_check_requires_explicit_fetch(tmp_path, monkeypatch):
    manifest, _ = inputs(tmp_path, monkeypatch)
    (tmp_path/manifest['entries'][0]['input_path']).unlink()
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', lambda *a, **k: pytest.fail('implicit download'))
    with pytest.raises(FileNotFoundError, match='development.firmware init k750i'):
        firmware.firmware(tmp_path)


def screen_inputs(tmp_path, monkeypatch):
    manifest, spec_path = inputs(tmp_path, monkeypatch)
    # Cover the real K750 FS geometry with controlled, authenticated bytes.
    fs_size = k750.SCREEN_STORAGE['fs_tail']['address']-0x45400000
    records = [(0x45400000+offset, b'\xa5'*min(65536, fs_size-offset))
               for offset in range(0, fs_size, 65536)]
    manifest['entries'][1] = image(tmp_path, 'fs', records)
    (k750.RESOURCES/'k750-r1db001-cid49-manifest.json').write_text(json.dumps(manifest))
    logical = struct.pack('<I', 2)+struct.pack('<BHI', 0, 0x1234, 3)+b'\x00\xff\x80'
    logical += struct.pack('<BHI', 2, 0x5678, 2)+b'\x01\x02'
    backup = tmp_path/'fw/gdfs.bin'
    backup.write_bytes(logical)
    entry = {'input_path': 'fw/gdfs.bin', 'size_bytes': len(logical), 'sha256': digest(logical)}
    spec = json.loads(spec_path.read_text())
    spec.update({'scope': 'k750-screen-compatibility',
        'k750_screen_inputs': {**copy.deepcopy(k750.SCREEN_STORAGE), 'gdfs_backup': entry},
        'nand': copy.deepcopy(k750.NAND_SCREEN),
        'dtcm_reset_record': {'profile': 'k750-r1db001-cold-zero-reset-record',
                              'address': 0x4200ffdc, 'size': 28, 'fill': 0},
        'hash_accelerator': {'profile': 'sha1-fifo-immediate-8040'}})
    spec_path.write_text(json.dumps(spec))
    source_path = firmware.RESOURCES/'k750-r1db001-cid49-source.json'
    source = json.loads(source_path.read_text())
    source['gdfs_backup'] = {**entry, 'url': 'https://firmware.example/K750_GDFS.bin'}
    source_path.write_text(json.dumps(source))
    return manifest, spec_path, entry, logical


def test_screen_inputs_preserve_original_code_and_report_reconstructed_storage(tmp_path, monkeypatch):
    manifest, spec, entry, logical = screen_inputs(tmp_path, monkeypatch)
    output = tmp_path/'screen'
    prepared, _ = prepare(tmp_path, spec, output)
    state = json.loads(prepared.read_text())
    seeds = state['regions'][0]['seeds']
    assert state['regions'][1]['seeds'] == []
    assert state['executable_ranges'] == [
        {'start': start, 'end': end} for start, end in manifest['entries'][0]['ranges']+[[RAM, RAM+8]]]
    original_seeds = [row for row in seeds if row['address'] < 0x45400000]
    assert b''.join(Path(row['file']).read_bytes() for row in original_seeds) == b''.join(
        data for _, data in k750.container(tmp_path, manifest['entries'][0]))
    tail = next(row for row in seeds if row['address'] == 0x45e00404)
    raw = bytearray(Path(tail['file']).read_bytes())
    for address in k750.SCREEN_STORAGE['fs_tail']['footer_addresses']:
        offset = address-0x45e00404
        assert raw[offset:offset+2] == bytes.fromhex('5412')
        raw[offset:offset+2] = b'\xff\xff'
    assert raw == b'\xff'*len(raw)
    gdfs = next(row for row in seeds if row['address'] == 0x45f00000)
    assert Path(gdfs['file']).read_bytes() == k750.bank_image(logical)[0]
    assert Path(state['nand']['nand_main_image']).read_bytes() == b'\xff'*0x2000000
    assert Path(state['nand']['nand_spare_image']).read_bytes() == b'\xff'*0x100000
    report = json.loads(artifact(output, 'inputs.json').read_text())
    assert report['screen_storage']['gdfs_backup']['source_sha256'] == entry['sha256']
    assert report['screen_storage']['gdfs_backup']['payloads_preserved']
    assert not report['screen_storage']['gdfs_backup']['physical_dump']
    assert 'hash_accelerator' in report['selected_engine_models']


def test_screen_backup_change_is_rejected_before_preparation(tmp_path, monkeypatch):
    _, spec, entry, _ = screen_inputs(tmp_path, monkeypatch)
    (tmp_path/entry['input_path']).write_bytes(b'changed backup')
    with pytest.raises(ValueError, match='GDFS backup identity differs'):
        prepare(tmp_path, spec, tmp_path/'changed')


def test_gdfs_fetch_authenticates_and_does_not_replace_existing_inputs(tmp_path, monkeypatch):
    _, _, entry, logical = screen_inputs(tmp_path, monkeypatch)
    path = tmp_path/entry['input_path']
    path.unlink()
    calls = []
    def fetch(url, timeout):
        calls.append(url)
        return io.BytesIO(logical)
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', fetch)
    with pytest.raises(FileNotFoundError, match='Missing K750 GDFS backup'):
        firmware.firmware(tmp_path, include_gdfs=True)
    assert calls == []
    paths = firmware.firmware(tmp_path, download=True, include_gdfs=True)
    assert paths[-1] == path and path.read_bytes() == logical and len(calls) == 1
    firmware.firmware(tmp_path, include_gdfs=True)
    path.write_bytes(b'damaged backup')
    with pytest.raises(ValueError, match='GDFS backup identity differs'):
        firmware.firmware(tmp_path, download=True, include_gdfs=True)
    assert path.read_bytes() == b'damaged backup' and len(calls) == 1


def test_bad_gdfs_download_is_not_published(tmp_path, monkeypatch):
    _, _, entry, _ = screen_inputs(tmp_path, monkeypatch)
    path = tmp_path/entry['input_path']
    path.unlink()
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', lambda *a, **k: io.BytesIO(b'bad download'))
    with pytest.raises(ValueError, match='GDFS download differs'):
        firmware.firmware(tmp_path, download=True, include_gdfs=True)
    assert not path.exists() and not list(path.parent.glob('.download-*'))


def customization_zip(tmp_path, members=None):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as archive:
        for name, data in (members or {
            'tpa/preset/custom/customize.xml': b'<customization/>',
            'tpa/preset/custom/Root.cer': b'original-certificate',
            'IFS/SETTINGS/DISPLAY/dispdriver0.dat': b'preserve-existing-driver',
        }).items():
            archive.writestr(name, data)
    path = tmp_path/'fw/custom.zip'
    path.parent.mkdir(exist_ok=True)
    path.write_bytes(stream.getvalue())
    return {'input_path': 'fw/custom.zip', 'size_bytes': path.stat().st_size,
            'sha256': digest(path.read_bytes())}


def test_customization_archive_authenticates_members_and_leaves_drivers(tmp_path):
    entry = customization_zip(tmp_path)
    files, report = k750.customization_archive(tmp_path, entry)
    assert dict(files) == {'tpa/preset/custom/Root.cer': b'original-certificate',
                           'tpa/preset/custom/customize.xml': b'<customization/>'}
    assert report['omitted_files'] == ['IFS/SETTINGS/DISPLAY/dispdriver0.dat']
    assert report['driver_partitions_preserved']
    path = tmp_path/entry['input_path']
    path.write_bytes(path.read_bytes()+b'x')
    with pytest.raises(ValueError, match='archive identity differs'):
        k750.customization_archive(tmp_path, entry)


@pytest.mark.parametrize('name', ['tpa/preset/custom/../bad.xml',
                                 'tpa/preset/custom/..', 'other/customize.xml'])
def test_customization_archive_rejects_bad_paths_and_missing_xml(tmp_path, name):
    entry = customization_zip(tmp_path, {name: b'<customization/>'})
    with pytest.raises(ValueError, match='customization member|Missing K750 customize.xml'):
        k750.customization_archive(tmp_path, entry)


def test_customization_fetch_is_authenticated_and_then_offline(tmp_path, monkeypatch):
    inputs(tmp_path, monkeypatch)
    entry = customization_zip(tmp_path)
    source_path = firmware.RESOURCES/'k750-r1db001-cid49-source.json'
    source = json.loads(source_path.read_text())
    source['customization_archive'] = {**entry, 'url': 'https://firmware.example/custom.zip'}
    source_path.write_text(json.dumps(source))
    path = tmp_path/entry['input_path']
    raw = path.read_bytes()
    path.unlink()
    calls = []
    def fetch(url, timeout):
        calls.append(url)
        return io.BytesIO(raw)
    monkeypatch.setattr(firmware.urllib.request, 'urlopen', fetch)
    firmware.firmware(tmp_path, download=True, include_customization=True)
    assert path.read_bytes() == raw and len(calls) == 1
    firmware.firmware(tmp_path, download=True, include_customization=True)
    assert len(calls) == 1
    path.write_bytes(b'damaged')
    with pytest.raises(ValueError, match='archive identity differs'):
        firmware.firmware(tmp_path, download=True, include_customization=True)
    assert path.read_bytes() == b'damaged' and len(calls) == 1
