"""K750's two PDI read programs return the actual selected R61500 register."""
import copy
import json

import pytest

from support.fixtures import base_state, qemu_run, words
from support.paths import RESOURCES
from test_native_checkpoint import engine, migration, stopped

PROGRAMS = json.loads((RESOURCES / 'pdi-reset-programs.json').read_text())['programs']
READ_PROFILE = {'profile': 'r61500-read-write-packets-retained-latches'}


def write(address, value):
    # LDR value/address, STR, then branch over the two literals.
    return [0xE59F0008, 0xE59F1008, 0xE5810000, 0xEA000001, value, address]


def read_state(change=None):
    code = []
    for kind, register in [(3, 2), (2, 3)]:
        program = bytearray.fromhex(PROGRAMS[kind]['bytes_hex'])
        if change == 'program':
            program[0] ^= 1
        for offset in range(0, len(program), 4):
            code += write(0xF700011C + offset,
                          int.from_bytes(program[offset:offset + 4], 'little'))
        index = 1 if change == 'unknown_register' else 0x22 if change == 'pixel_read' else 0
        for address, value in [
            (0xF70001FC, (64 if change == 'selector' else 28) << 16),
            (0xF70001F8, 0x1500),
            (0xF700017C, index << 24),
            (0xF700018C, 0xC0000000 if change == 'mask' else 0),
            (0xF70001DC, 3),
        ]:
            code += write(address, value)
        # Original driver's bit0 poll, followed by extraction of bits8..15.
        code += [0xE59F1010, 0xE5910000 | register << 12,
                 0xE3100001 | register << 16, 0x1AFFFFFC,
                 0xE1A00420 | register << 12 | register,
                 0xEA000000, 0xF70001F4]
    code += [0xE1834402, 0xE5854000]  # Combine high/low bytes and stop at r5.
    state = base_state(words(*code), limit=1000)
    state['registers'][5] = 0x60000000
    state['pdi'] = ({'profile': 'r61500-write-packets-retained-latches'}
                    if change == 'disabled' else READ_PROFILE.copy())
    return state


@pytest.mark.parametrize('guarded', [False, True])
def test_device_code_is_read_as_high_then_low_byte(tmp_path, guarded):
    state = read_state()
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path / 'read', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['registers'][2:5] == [0x15, 0, 0x1500]
    assert [(event['trigger'], event['bytes'], event['read_byte'])
            for event in result['pdi']['transfers']] == [(3, [0, 0], 0x15), (3, [0, 0], 0)]


@pytest.mark.parametrize('change,reason', [
    ('disabled', 'pdi_trigger_unmodeled'),
    ('program', 'pdi_read_program_unmodeled'),
    ('selector', 'pdi_read_selector_unmodeled'),
    ('mask', 'pdi_read_index_unmodeled'),
    ('unknown_register', 'display_panel_read_unknown'),
    ('pixel_read', 'display_panel_read_unknown'),
])
def test_unsupported_read_stops_before_panel_effect(tmp_path, change, reason):
    result = qemu_run(read_state(change), tmp_path / 'rejected', trace=False)
    assert result['stop']['reason'] == reason
    assert result['stop']['address'] == 0xF70001DC
    assert result['pdi']['transfers'] == []
    assert result['pdi']['busy_deadline_ns'] == 0


def test_pending_read_result_survives_checkpoint(tmp_path):
    state = read_state()
    first = copy.deepcopy(state)
    first['instruction_limit'] = 64  # Immediately after the first read trigger.
    checkpoint = tmp_path / 'read.vmstate'
    with engine(first, tmp_path / 'prefix') as vm:
        start = stopped(vm)
        assert start['pdi']['transfers'][-1]['read_byte'] == 0x15
        migration(vm, 'migrate', checkpoint)
    state['instruction_limit'] -= 64
    with engine(state, tmp_path / 'restored', incoming=True) as vm:
        migration(vm, 'migrate-incoming', checkpoint)
        result = stopped(vm)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['registers'][4] == 0x1500
