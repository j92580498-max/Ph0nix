"""Firmware-scoped flash probes execute through native ARM/Thumb and NOR I/O."""
import copy
import json
import struct

import pytest

from support.fixtures import base_state, qemu_run, words
from support.paths import BASELINE_MODELS

SITES = {'w800-r1l002': (0x4C002C32, 0x4C002C3A, 0x4C002C44),
         'k750-r1db001': (0x4C002CEE, 0x4C002CF6, 0x4C002D00)}


def probe_state(profile, case, *, selected=None):
    cfi = case.startswith('query')
    index = {'array55': 0, 'array555': 1, 'queryaa': 0, 'queryaaa': 1, 'query10': 2}[case]
    pc = SITES[profile][index]
    factor = 2 if cfi and index != 2 else 1
    address = {'array55': 0x44000055, 'array555': 0x44000555,
               'queryaa': 0x440000AA, 'queryaaa': 0x44000AAA, 'query10': 0x44000010}[case]
    # Query entry writes 98 through the byte bridge, then restores r1 to BASE.
    preface = words(0xE5C14000, 0xE2411055, 0xE12FFF1C) if cfi else words(0xE12FFF1C)
    state = base_state(preface, limit=32)
    state['registers'][0] = state['registers'][3] = factor
    state['registers'][1] = 0x44000055 if cfi else 0x44000000
    state['registers'][2] = state['registers'][5] = address
    state['registers'][4] = 0x98
    state['registers'][7] = 0x10
    state['registers'][12] = pc | 1
    destination = (4, 6, 7)[index]
    # The next byte store deliberately stops at unmapped address r0 (1 or 2).
    code = struct.pack('<2H', (0x7814, 0x782E, 0x5DCF)[index], 0x7000 | destination)
    state['regions'].extend([
        {'name': 'fixture.probe', 'address': 0x4C002000, 'size': 4096,
         'readonly': True, 'seeds': [{'address': pc, 'data': code}]},
        {'name': 'fixture.nor', 'address': 0x44000000, 'size': 0x2000000,
         'readonly': True, 'seeds': []}])
    state['executable_ranges'].append({'start': pc, 'end': pc+len(code)})
    state['nor_model'] = copy.deepcopy(json.loads(BASELINE_MODELS.read_text())['nor_model'])
    state['nor_byte_bridge'] = {'profile': 'steered-query-reset-zero-high'}
    state['nor_probe_placeholders'] = {'value': 255, 'x16_retry': True}
    # Omitted firmware selection preserves the historical W800 contract.
    selected = selected or profile
    if selected != 'w800-r1l002':
        state['nor_probe_placeholders']['profile'] = selected
    return state, destination, address


@pytest.mark.parametrize('profile', SITES)
@pytest.mark.parametrize('guarded', [True, False])
@pytest.mark.parametrize('case', ['array55', 'array555', 'queryaa', 'queryaaa', 'query10'])
def test_native_probe_inputs_use_selected_original_sites(tmp_path, profile, guarded, case):
    state, destination, _ = probe_state(profile, case)
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['registers'][destination] == (0 if case == 'query10' else 255)


@pytest.mark.parametrize('profile', SITES)
@pytest.mark.parametrize('case', ['array55', 'query10'])
def test_other_firmware_sites_keep_unknown_flash_guard(tmp_path, profile, case):
    other = next(name for name in SITES if name != profile)
    state, _, address = probe_state(profile, case, selected=other)
    result = qemu_run(state, tmp_path/'run', trace=False)
    assert result['stop']['reason'] == ('nor_read_unknown_array' if case == 'array55' else 'nor_unmodeled_cfi_offset')
    assert result['stop']['address'] == address


@pytest.mark.parametrize('profile', SITES)
def test_probe_placeholder_preserves_supplied_flash_byte(tmp_path, profile):
    state, destination, address = probe_state(profile, 'array55')
    state['regions'][-1]['seeds'] = [{'address': address, 'data': b'\x37'}]
    result = qemu_run(state, tmp_path/'run', trace=False)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['registers'][destination] == 0x37
