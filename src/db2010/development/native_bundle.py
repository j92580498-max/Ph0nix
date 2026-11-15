"""Freeze audited compatibility inputs for native hosts; omit MAIN/FS bytes."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED

from ..paths import ROOT, K750_EXPERIMENT, RESOURCES
from ..preparation.inputs import prepare


def bundle(root, output):
    with tempfile.TemporaryDirectory(prefix='k750-native-bundle-') as temporary:
        prepared, _ = prepare(root, K750_EXPERIMENT, Path(temporary), recording=False)
        state = json.loads(prepared.read_text())
        catalog = json.loads((RESOURCES/'k750-r1db001-cid49-manifest.json').read_text())
        compatibility = json.loads((RESOURCES/'k750-r1db001-cid49-native-compatibility.json').read_text())
        main = next(entry for entry in catalog['entries'] if entry['id'] == 'main')
        if (compatibility['format_version'] != 1 or compatibility['main_sha256'] != main['sha256']):
            raise ValueError('Native Java compatibility MAIN identity differs')
        nor = state['regions'][0]
        fs = next(entry for entry in catalog['entries'] if entry['id'] == 'fs')
        fs_seed = next(seed for seed in nor['seeds'] if seed['address'] == fs['base'])
        fs_bytes = Path(fs_seed['file']).read_bytes()
        from ..preparation.filesystem import Fat16
        picture = (RESOURCES/'ph0nix-puzzle.png').read_bytes()
        disk = Fat16(fs_bytes)
        disk.install_picture('ph0nix.png', picture)
        fs_bytes = disk.render()
        # Authenticate stock FS before applying the same declared CDA/FAT
        # provisioning as Python preparation, plus the native test picture.
        # Importers never modify the user's source firmware files.
        from ..preparation.k750 import container
        chunks = container(root, fs)
        stock = bytearray([255]) * len(fs_bytes)
        for (address, _end), data in chunks:
            stock[address-fs['base']:address-fs['base']+len(data)] = data
        patches = []
        start = None
        for offset, (before, after) in enumerate(zip(stock, fs_bytes)):
            if before != after:
                if start is None:
                    start = offset
            elif start is not None:
                patches.append({'address': fs['base']+start,
                    'original_hex': stock[start:offset].hex(), 'value_hex': fs_bytes[start:offset].hex()})
                start = None
        if start is not None:
            patches.append({'address': fs['base']+start,
                'original_hex': stock[start:].hex(), 'value_hex': fs_bytes[start:].hex()})
        gdfs_seed = next(seed for seed in nor['seeds'] if seed['address'] == 0x45f00000)
        files = {
            'gdfs.bin': Path(gdfs_seed['file']).read_bytes(),
            'nand-main.bin': Path(state['nand']['nand_main_image']).read_bytes(),
            'nand-spare.bin': Path(state['nand']['nand_spare_image']).read_bytes(),
            'nand-known-spare.bin': Path(state['nand']['prepared_spare']).read_bytes(),
        }
        nor['seeds'] = []
        state['execution_proofs'] = []
        state.pop('instruction_limit', None)
        state['research_guards'] = False
        state['record_diagnostics'] = False
        state['simif']['card_profile'] = 'ph0nix-test-sim'
        state['recording_history'] = {'profile':'startup64-recent64-irq512'}
        for key, name in [('nand_main_image','nand-main.bin'), ('nand_spare_image','nand-spare.bin'),
                          ('prepared_spare','nand-known-spare.bin')]:
            state['nand'][key] = name
        manifest = {
            'format_version': 2, 'profile': 'k750-r1db001-native-ui',
            'catalog': catalog, 'fs': {'base': fs['base'], 'size': len(fs_bytes), 'patches': patches,
                'stock_sha256': hashlib.sha256(stock).hexdigest(),
                'provisioned_sha256': hashlib.sha256(fs_bytes).hexdigest()},
            'compatibility_files': {name: {'size':len(data), 'sha256':hashlib.sha256(data).hexdigest()}
                                    for name, data in files.items()},
            'main_and_fs_included': False,
            'main_patches': compatibility['main_patches'],
            'sim_profile': 'ph0nix-test-sim; IMSI 001010123456789; SPN and CPHS operator name ph0nix; no cellular service',
            'customization': 'CDA102338/36 EMEA_1; explicit FAT allocation and file installation after stock FS authentication',
            'user_pictures': [{'path':'tpa/user/image/ph0nix.png', 'size':len(picture),
                               'sha256':hashlib.sha256(picture).hexdigest()}],
            'assumptions': ['explicit R1 reload in the Java software fallback; input MAIN remains authenticated and unchanged',
                            'audited GDFS bank with emulator-derived dynamic SHA1 checksums',
                            'matching assumed OTP identity', 'scoped synthetic ETX status',
                            'synthetic ROM hash initialization and immediate SHA1/KGEN model',
                            'erased storage metadata and installed authenticated customization files',
                            'synthetic DSP/audio, SIM and transport peers'],
        }
        files['state-template.json'] = (json.dumps(state, sort_keys=True)+'\n').encode()
        files['manifest.json'] = (json.dumps(manifest, sort_keys=True)+'\n').encode()
        output.parent.mkdir(parents=True, exist_ok=True)
        temporary = output.with_suffix('.tmp')
        with ZipFile(temporary, 'w', compression=ZIP_DEFLATED, compresslevel=9) as archive:
            for name, data in sorted(files.items()):
                info = ZipInfo(name, (1980,1,1,0,0,0))
                info.compress_type = ZIP_DEFLATED
                archive.writestr(info, data)
        temporary.replace(output)
        return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=ROOT)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    manifest = bundle(args.repo.resolve(), args.output.resolve())
    print(json.dumps({'output': str(args.output), 'bytes': args.output.stat().st_size,
                      'main_and_fs_included': manifest['main_and_fs_included']}))


if __name__ == '__main__':
    main()
