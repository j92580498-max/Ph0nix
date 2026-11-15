"""Original K750 peripheral routines execute against native DB2010 devices."""
import json

import pytest

from db2010.paths import K750_PREFIX_EXPERIMENT
from support.fixtures import base_state, qemu_run, words
from support.paths import RESOURCES


@pytest.mark.parametrize('guarded', [True, False])
@pytest.mark.parametrize('ticks', [0, 255])
def test_original_k750_rtc_getter_reads_stable_low_byte(tmp_path, guarded, ticks):
    original = json.loads((RESOURCES/'k750-rtc-original.json').read_text())
    address = original['start']
    # The original BX LR returns to an ARM byte store at unmapped address r3.
    state = base_state(words(0xE5C30000), entry=address, cpsr=0xF3, limit=32)
    state['registers'][3] = 1
    state['registers'][14] = 0x1000
    state['regions'].append({'name': 'fixture.k750-rtc',
        'address': address & ~0xFFF, 'size': 4096, 'readonly': True,
        'seeds': [{'address': address, 'data': bytes.fromhex(original['hex'])}]})
    state['executable_ranges'].append({'start': address, 'end': address+24})
    state['rtc_counter'] = json.loads(K750_PREFIX_EXPERIMENT.read_text())['rtc_counter']
    state['rtc_counter']['initial_ticks'] = ticks
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['registers'][0] == ticks
    assert result['rtc']['control'] == 0


@pytest.mark.parametrize('guarded', [True, False])
def test_original_k750_i2c_initialization_establishes_controller_state(tmp_path, guarded):
    original = json.loads((RESOURCES/'k750-i2c-original.json').read_text())
    code, literal = original['initialize'], original['literal']
    address = code['start']
    state = base_state(words(0xE5C70000), entry=address, cpsr=0xF3, limit=32)
    state['registers'][7] = 1
    state['registers'][14] = 0x1000
    state['regions'].append({'name': 'fixture.k750-i2c',
        'address': address & ~0xFFF, 'size': 4096, 'readonly': True,
        'seeds': [
            {'address': address, 'data': bytes.fromhex(code['hex'])},
            # Return after the ten original initialization instructions.
            {'address': code['end'], 'data': bytes.fromhex('7047')},
            {'address': literal['address'], 'data': bytes.fromhex(literal['hex'])}]})
    state['executable_ranges'].append({'start': address, 'end': code['end']+2})
    state['i2c_controller'] = json.loads(K750_PREFIX_EXPERIMENT.read_text())['i2c_controller']
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['i2c']['clock'] == 0x60
    assert result['i2c']['control'] == result['i2c']['pending_operation'] == 0
    assert result['i2c']['transactions'] == 0


@pytest.mark.parametrize('guarded', [True, False])
@pytest.mark.parametrize('initial', [None, 0, 0xc0])
def test_original_k750_simif_setup_preserves_declared_reset_bits(tmp_path, guarded, initial):
    original = json.loads((RESOURCES/'k750-simif-original.json').read_text())
    code, literal = original['setup'], original['literal']
    address = code['start']
    state = base_state(words(0xE5C30000), entry=address, cpsr=0xF3, limit=40)
    state['registers'][3] = 1
    state['registers'][5] = 0xf9060004
    state['registers'][14] = 0x1000
    state['regions'].append({'name': 'fixture.k750-simif',
        'address': address & ~0xFFF, 'size': 4096, 'readonly': True,
        'seeds': [{'address': address, 'data': bytes.fromhex(code['hex'])},
                  {'address': code['end'], 'data': bytes.fromhex('7047')},
                  {'address': literal['address'], 'data': bytes.fromhex(literal['hex'])}]})
    state['executable_ranges'].append({'start': address, 'end': code['end']+2})
    state['simif'] = {'profile': 'pmb7850-gsm-stub-native-simif', 'byte_ns': 100000}
    if initial is not None:
        state['simif']['setup_initial_value'] = initial
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, reset=True, research_guards=guarded)
    assert result['stop']['reason'] == ('simif_register_read_unknown' if initial is None else 'write_unmapped')
    accesses = [(row['write'], row['value']) for row in result['simif']['events'] if row['offset'] == 2]
    assert accesses == ([] if initial is None else [(False, initial), (True, initial | 0x40),
                                                   (False, initial | 0x40), (True, 0x40)])
    assert result['after_reset']['simif'] == result['initial']['simif']
