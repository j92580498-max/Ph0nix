"""Authenticate and prepare sparse original K750 R1DB001 CID49 firmware."""
import json
import io
import struct
import zipfile
from pathlib import Path

from ..diagnostics.artifacts import artifact, external_inputs
from ..paths import RESOURCES
from .cache import cached_input
from .inputs import MODELS, digest, load_json, nand_spare_seeds, reset_entry, reset_record_input, resource
from .gdfs import bank_image, GDFS_BASE
from .filesystem import Fat16, FS_BASE

NAND_PROBE = {
    'profile': 'samsung-256m-x8-reset-id-status', 'address': 0x50000000,
    'manufacturer_id': 0xEC, 'device_id': 0x35,
    'completion': 'immediate-ready', 'write_protected': False,
}
NAND_SCREEN = {
    **NAND_PROBE,
    'spare_read': {'profile': 'samsung-x8-read2-unknown-contents'},
    'spare_program': {'profile': 'samsung-x8-known-spare-lifecycle-no-wear'},
    'synthetic_spare': {'profile': 'all-spare-ff-valid-footers'},
    'block_erase': {'profile': 'samsung-x8-block-erase-immediate'},
    'main_array': {'profile': 'samsung-x8-known-main-read-program'},
    'copy_back': {'profile': 'samsung-x8-known-page-copyback-no-wear'},
}
SCREEN_STORAGE = {
    'profile': 'archive-gdfs-fresh-storage',
    'fs_tail': {'address': 0x45E00404, 'end': 0x45F00000, 'fill': 255,
                'footer_hex': '5412',
                'footer_addresses': [0x45E3FFFE, 0x45E7FFFE, 0x45EBFFFE, 0x45EFFFFE]},
    'nand': {'main_bytes': 0x2000000, 'spare_bytes': 0x100000, 'fill': 255},
}
DYNAMIC_CHECKSUMS = {'profile': 'sha1-static-prefix-dynamic6-14'}
K750_ETX = {
    'profile': 'gdfs-static-8040-status-success', 'callers': 'k750-r1db001',
    'expected_request_hex': (
        '8ee639a56df194fda96feb86d082659ba4f4e6bd3c000d14abfd1a6153898cc82'
        '052d0c86207a6d7b69585e53271e14006f0265921d02d4d736cdc580b0b95a1f9'
        'fa521545905140f169f655cbcdaa485873'),
}


def gdfs_backup(root, entry, input_path=None, *, emulator_dynamic_sha1=False):
    """Authenticate a logical backup before reconstructing storage metadata."""
    if (not isinstance(entry, dict) or set(entry) != {'input_path', 'size_bytes', 'sha256'}
            or type(entry['size_bytes']) is not int or not 4 <= entry['size_bytes'] <= 0x100000):
        raise ValueError('Unsupported K750 GDFS backup identity')
    path = resource(root, entry['input_path']) if input_path is None else input_path
    try:
        backup = path.read_bytes()
    except FileNotFoundError as error:
        raise FileNotFoundError(
            f'Missing K750 GDFS backup: {path}. '
            'Run python -m db2010.development.firmware init k750i') from error
    if len(backup) != entry['size_bytes'] or digest(backup) != entry['sha256']:
        raise ValueError('K750 GDFS backup identity differs')
    return bank_image(backup, emulator_dynamic_sha1=emulator_dynamic_sha1)


def customization_archive(root, entry, input_path=None):
    """Authenticate the ZIP and read its customization members without extraction."""
    if (not isinstance(entry, dict) or set(entry) != {'input_path', 'size_bytes', 'sha256'}
            or type(entry['size_bytes']) is not int or not 1 <= entry['size_bytes'] <= 0x1000000):
        raise ValueError('Unsupported K750 customization archive identity')
    path = resource(root, entry['input_path']) if input_path is None else input_path
    try:
        raw = path.read_bytes()
    except FileNotFoundError as error:
        raise FileNotFoundError(
            f'Missing K750 customization archive: {path}. '
            'Run python -m db2010.development.firmware init k750i') from error
    if len(raw) != entry['size_bytes'] or digest(raw) != entry['sha256']:
        raise ValueError('K750 customization archive identity differs')
    files, omitted, identities = {}, [], []
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        for item in sorted(archive.infolist(), key=lambda item: item.filename):
            if item.is_dir():
                continue
            name = item.filename
            if not name.startswith('tpa/preset/custom/'):
                omitted.append(name)
                continue
            parts = name.split('/')
            if (len(parts) != 4 or parts[-1] in ('', '.', '..')
                    or '\\' in name or name.casefold() in files
                    or not 0 < item.file_size <= 0x100000):
                raise ValueError('Unsupported K750 customization member')
            data = archive.read(item)
            files[name.casefold()] = (name, data)
            identities.append({'path': name, 'size_bytes': len(data), 'sha256': digest(data)})
    if 'tpa/preset/custom/customize.xml' not in files:
        raise ValueError('Missing K750 customize.xml in archive')
    return list(files.values()), {
        'source_bytes': len(raw), 'source_sha256': digest(raw),
        'installed_files': identities, 'omitted_files': omitted,
        'driver_partitions_preserved': True,
    }


def screen_storage(root, input_root, spec, fs, seeds):
    """Declared screen profile inputs, separate from the sparse prefix contract."""
    inputs = spec.get('k750_screen_inputs')
    if (not isinstance(inputs, dict)
            or set(inputs) - {'customization_archive', 'dynamic_checksums'} != {*SCREEN_STORAGE, 'gdfs_backup'}
            or any(inputs[key] != value for key, value in SCREEN_STORAGE.items())
            or ('dynamic_checksums' in inputs and inputs['dynamic_checksums'] != DYNAMIC_CHECKSUMS)
            or fs['ranges'] != [[0x45400000, SCREEN_STORAGE['fs_tail']['address']]]
            or spec.get('nand') != NAND_SCREEN):
        raise ValueError('Unsupported K750 screen storage inputs')
    tail = inputs['fs_tail']
    raw = bytearray([tail['fill']]) * (tail['end'] - tail['address'])
    for address in tail['footer_addresses']:
        raw[address-tail['address']:address-tail['address']+2] = bytes.fromhex(tail['footer_hex'])
    customization = None
    if 'customization_archive' in inputs:
        files, customization = customization_archive(input_root, inputs['customization_archive'])
        fs_seed = next(seed for seed in seeds if seed['address'] == FS_BASE)
        stock = Path(fs_seed['file']).read_bytes() + bytes(raw)
        volume = Fat16(stock)
        for name, data in files:
            volume.install(name, data)
        provisioned = volume.render()
        fs_seed['file'] = str(cached_input(root, provisioned))
        customization.update({'stock_fs_sha256': digest(stock),
                              'prepared_fs_sha256': digest(provisioned),
                              'prepared_fs_bytes': len(provisioned)})
    else:
        seeds.append({'address': tail['address'], 'file': str(cached_input(root, raw))})
    gdfs, gdfs_report = gdfs_backup(input_root, inputs['gdfs_backup'],
                                  emulator_dynamic_sha1='dynamic_checksums' in inputs)
    seeds.append({'address': GDFS_BASE, 'file': str(cached_input(root, gdfs))})
    nand = dict(NAND_SCREEN)
    nand['prepared_spare'] = str(cached_input(root, nand_spare_seeds(nand['synthetic_spare']['profile'])))
    nand_hashes = {}
    for key, size_key in [('nand_main_image', 'main_bytes'), ('nand_spare_image', 'spare_bytes')]:
        image = bytes([inputs['nand']['fill']]) * inputs['nand'][size_key]
        nand[key] = str(cached_input(root, image))
        nand_hashes['main_sha256' if size_key == 'main_bytes' else 'spare_sha256'] = digest(image)
    report = {'fs_tail': {**tail, 'sha256': digest(raw), 'synthetic': True},
              'gdfs_backup': gdfs_report,
              **({'customization': customization} if customization else {}),
              'nand': {**inputs['nand'], 'physical_dump': False, **nand_hashes}}
    return nand, report


def container(root, entry, input_path=None):
    """Authenticate the entire cataloged file, including its opaque trailer."""
    path = resource(root, entry['input_path']) if input_path is None else input_path
    try:
        data = path.read_bytes()
    except FileNotFoundError as error:
        raise FileNotFoundError(
            f'Missing K750 {entry["id"]} container: {path}. '
            'Run python -m db2010.development.firmware init k750i '
            'or select the audited files with --main and --fs.') from error
    if (len(data) != entry['size'] or digest(data) != entry['sha256']
            or data[:4] != bytes.fromhex('babe0003')
            or struct.unpack_from('<I', data, 0x2e8)[0] != len(entry['segments'])):
        raise ValueError('K750 container identity differs')
    chunks, ranges = [], []
    offset = entry['segments'][0]['offset'] - 8
    for row in entry['segments']:
        address, size = row['address'], row['length']
        if (not 0 < size <= 65536 or row['offset'] != offset + 8
                or offset + 8 + size > len(data)
                or struct.unpack_from('<II', data, offset) != (address, size)
                or not entry['base'] <= address < address + size <= entry['end']
                or ranges and address < ranges[-1][1]):
            raise ValueError('K750 record mapping differs')
        raw = data[row['offset']:row['offset'] + size]
        if ranges and ranges[-1][1] == address:
            ranges[-1][1] += size
            chunks[-1].append(raw)
        else:
            ranges.append([address, address + size])
            chunks.append([raw])
        offset += 8 + size
    chunks = [b''.join(parts) for parts in chunks]
    if (ranges != entry['ranges'] or offset != entry['record_stream_end']
            or digest(b''.join(chunks)) != entry['payload_sha256']):
        raise ValueError('K750 payload coverage differs')
    return list(zip(ranges, chunks))


def prepare_k750(root, spec_path, spec, output, *, images, recording, input_root=None):
    input_root = root if input_root is None else input_root
    screen = spec.get('scope') == 'k750-screen-compatibility'
    unsupported = ('nor_erased_tail', 'nor_gdfs_image') if screen else (
            'nor_erased_tail', 'nor_gdfs_image', 'dtcm_reset_record',
            'f500_command_capture', 'synthetic_etx', 'synthetic_kgen', 'synthetic_sha',
            'hash_accelerator', 'k750_screen_inputs')
    if images.keys() - {'main', 'fs'} or 'firmware_images' in spec or any(k in spec for k in unsupported):
        raise ValueError('K750 requires its audited MAIN/FS containers; GDFS, NAND and synthetic ROM inputs are unsupported')
    if not screen and 'nand' in spec and spec['nand'] != NAND_PROBE:
        raise ValueError('K750 NAND supports unseeded reset/ID/status only')
    if screen:
        if 'synthetic_etx' in spec and spec['synthetic_etx'] != K750_ETX:
            raise ValueError('K750 screen requires its scoped original ETX request')
        for name in ('synthetic_sha', 'synthetic_kgen'):
            if name in spec and spec[name] != {
                    'profile': 'sha1-le92-init-update-final-8040', 'callers': 'k750-r1db001'}:
                raise ValueError('K750 screen requires its identified ROM hash callers')
        if spec.get('hash_accelerator') != {'profile': 'sha1-fifo-immediate-8040'}:
            raise ValueError('Unsupported K750 screen hash accelerator')
        reset_record_input(spec['dtcm_reset_record'])
    config_path = RESOURCES / spec['baseline']
    manifest_path = RESOURCES / 'k750-r1db001-cid49-manifest.json'
    config, manifest = load_json(config_path), load_json(manifest_path)
    main, fs = manifest['entries']
    if main['id'] != 'main' or fs['id'] != 'fs':
        raise ValueError('K750 direct-MAIN baseline differs')
    seeds, main_chunks, overrides = [], [], {}
    for entry in (main, fs):
        chunks = container(input_root, entry, images.get(entry['id']))
        if entry['id'] in images:
            overrides[entry['id']] = {
                'input_path': str(images[entry['id']]), 'size_bytes': entry['size'],
                'sha256': entry['sha256'], 'payload_sha256': entry['payload_sha256'],
                'validation': 'cataloged K750 container identity and sparse record mapping'}
        if entry is main:
            main_chunks = chunks
            reset_pc = reset_entry(chunks[0][1], [bounds for bounds, _ in chunks])
        for (start, end), raw in chunks:
            seeds.append({'address': start, 'file': str(cached_input(root, raw))})
    storage, nand = None, None
    if screen:
        nand, storage = screen_storage(root, input_root, spec, fs, seeds)
    regions = [{'name': 'db2010.nor-array', 'address': 0x44000000,
                'size': 0x2000000, 'readonly': True, 'seeds': seeds}]
    for row in config['memory']:
        region = {key: row[key] for key in ('name', 'address', 'size')}
        region['seeds'] = []
        if row['name'] == config['ram_initialization']['region']:
            region['assumed_fill'] = config['ram_initialization']['fill_byte']
        regions.append(region)
    proofs, proof_ranges = [], []
    if 'execution_contract' in config:
        contract_path = RESOURCES / 'k750-r1db001-cid49-execution-contract.json'
        if config['execution_contract'] != contract_path.name:
            raise ValueError('unsupported K750 execution contract')
        contract = load_json(contract_path)
        if contract['format_version'] != 1 or contract['payload_sha256'] != main['payload_sha256']:
            raise ValueError('K750 copy contract identity differs')
        for i, row in enumerate(contract['mappings']):
            address, source, size = (row[k] for k in ('address', 'source', 'size'))
            candidates = [raw[source-lo:source-lo+size] for (lo, hi), raw in main_chunks
                          if lo <= source < source+size <= hi]
            if (not 0x4C000000 <= address < address+size <= 0x4C800000
                    or len(candidates) != 1 or digest(candidates[0]) != row['sha256']):
                raise ValueError('K750 original-copy mapping differs')
            path = artifact(output, f'execution-proof-{i}.bin', create=True)
            path.write_bytes(candidates[0])
            proofs.append({'address': address, 'file': str(path)})
            proof_ranges.append([address, address+size])
        if recording:
            artifact(output, 'execution-provenance.json', create=True).write_text(
                json.dumps({'contract': contract, 'contract_sha256': digest(contract_path.read_bytes())},
                           indent=2) + '\n')
    if config['executable_ranges'] != main['ranges'] + proof_ranges:
        raise ValueError('K750 execution aperture differs from original-copy contract')
    state = {'format_version': 1, 'entry': reset_pc, 'cpsr': config['cpsr'],
             'sctlr': 0x90078, 'registers': [0] * 16,
             'instruction_limit': spec['instruction_limit'], 'regions': regions,
             'execution_proofs': proofs,
             'executable_ranges': [{'start': a, 'end': b} for a, b in config['executable_ranges']]}
    state.update({key: spec[key] for key in MODELS if key in spec})
    if screen:
        state['nand'] = nand
        state['dtcm_reset_record'] = spec['dtcm_reset_record']
    state_path = artifact(output, 'state.json', create=True)
    state_path.write_text(json.dumps(state, indent=2) + '\n')
    if recording:
        for name, path in [('experiment.json', spec_path), ('baseline.json', config_path),
                           ('manifest.json', manifest_path)]:
            artifact(output, name, create=True).write_bytes(path.read_bytes())
        artifact(output, 'inputs.json', create=True).write_text(json.dumps({
            'main': main, 'fs': fs, 'external_inputs': external_inputs(state, output),
            'manifest_sha256': digest(manifest_path.read_bytes()),
            'baseline_sha256': digest(config_path.read_bytes()),
            'experiment_sha256': digest(spec_path.read_bytes()),
            'assumptions': spec['assumptions'],
            'cli_image_overrides': overrides,
            'selected_engine_models': [key for key in MODELS if key in spec],
            'sparse_nor_policy': ('Authenticate MAIN/FS records and preserve MAIN gaps; '
                                  'explicit reconstructed GDFS and erased FS/NAND inputs.' if screen else
                                  'Seed authenticated records only; gaps and tails remain unknown.'),
            **({'screen_storage': storage,
                'dtcm_reset_record': reset_record_input(spec['dtcm_reset_record'])} if screen else {}),
            'ram_execution_policy': 'Only approved original-copy ranges; proof bytes never seed RAM.'
        }, indent=2) + '\n')
    return state_path, spec
