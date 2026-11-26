"""Native NOR and NAND inputs for committed flash tests."""
import json

from support.fixtures import base_state
from support.nor_fixtures import program as nor_program
from support.paths import BASELINE_MODELS


def storage_state(tmp_path, operations=()):
    code, limit, _ = nor_program(operations)
    state = base_state(code, limit=limit)
    profiles = json.loads(BASELINE_MODELS.read_text())
    state['nor_model'] = profiles['nor_model']
    state['nor_model']['initial_locked'] = False
    state['regions'].append({'name': 'fixture.nor', 'address': 0x44000000, 'size': 0x2000000,
        'readonly': True, 'seeds': [{'address': 0x44000000, 'data': b'prefix'},
                                  {'address': 0x453ffffe, 'data': b'\x12\x34'},
                                  {'address': 0x45f00000, 'data': b'\xff\xff'}]})
    seed = tmp_path/'spare-seeds.bin'; seed.write_bytes(b'\x00\x00\x0e\x54')
    state['nand'] = {**profiles['nand'], 'prepared_spare': str(seed),
        'block_erase': {'profile': 'samsung-x8-block-erase-immediate'},
        'main_array': {'profile': 'samsung-x8-known-main-read-program'}}
    return state

