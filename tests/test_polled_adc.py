"""Scoped PMIC ADC input works before asynchronous power initialization."""
import json

import pytest

from db2010.paths import K750_EXPERIMENT
from support.fixtures import base_state, qemu_run, words


def adc_state(*, command=0x6A, register=0x71):
    operations = [(2, 4), (4, 0x60), (0, 0), (2, 0),
        (2, 0x24), (0, 0x94), (2, 0), (0, 0x70), (2, 0),
        (0, command), (2, 0), (2, 0x10),
        (2, 0x24), (0, 0x94), (2, 0), (0, register), (2, 0),
        (2, 0x24), (0, 0x95), (2, 0), (2, 0)]
    code = []
    for offset, value in operations:
        code.extend([0xE3A00000 | value, 0xE1C100B0 | offset,
                     0xE1A00000, 0xE1A00000])
    # Receive one byte, finish STOP, then attempt an unmapped byte store.
    code.extend([0xE1D120B0, 0xE3A00010, 0xE1C100B2,
                 0xE1A00000, 0xE1A00000, 0xE5C34000])
    state = base_state(words(*code), limit=len(code)+8)
    state['registers'][1:4] = [0xF9040000, 0xAB, 1]
    state['i2c_controller'] = json.loads(K750_EXPERIMENT.read_text())['i2c_controller']
    return state


@pytest.mark.parametrize('guarded', [True, False])
@pytest.mark.parametrize('sample', [0, 130, 255, None])
def test_polled_adc_has_explicit_byte_or_unknown_input(tmp_path, guarded, sample):
    state = adc_state()
    state['i2c_controller']['polled_startup_adc'] = sample
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == ('i2c_polled_adc_input_unknown'
                                      if sample is None else 'write_unmapped')
    assert result['registers'][2] == (0xAB if sample is None else sample)
    assert result['i2c']['polled_startup_adc'] == sample
    assert result['i2c']['captured_writes'] == [
        {'slave': 0x4A, 'register': 0x70, 'value': 0x6A}]


@pytest.mark.parametrize('guarded', [True, False])
@pytest.mark.parametrize('change,reason', [
    ('omitted', 'i2c_slave_register_write_unmodeled'),
    ('command', 'i2c_slave_register_write_unmodeled'),
    ('register', 'i2c_slave_register_unmodeled')])
def test_polled_adc_keeps_undeclared_transactions_guarded(tmp_path, guarded, change, reason):
    state = adc_state(command=0x6B if change == 'command' else 0x6A,
                      register=0x72 if change == 'register' else 0x71)
    if change == 'omitted':
        state['i2c_controller'].pop('polled_startup_adc')
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == reason
    assert result['registers'][2] == 0xAB
