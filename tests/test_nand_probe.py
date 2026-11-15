"""NAND identification works without supplying any storage contents."""
import pytest

from db2010.preparation.k750 import NAND_PROBE
from support.fixtures import base_state, qemu_run, words


def probe_state(operations):
    code = []
    for kind, offset, value in operations:
        if kind == 'write':
            code.extend([0xE3A00000 | value, 0xE5C10000 | offset])
        else:
            code.append(0xE5D10000 | (value << 12) | offset)
    code.append(0xE5C65000)  # A rejected store after the accepted protocol.
    state = base_state(words(*code), limit=len(code)+1)
    state['registers'][1] = 0x50000000
    state['registers'][6] = 1
    state['nand'] = dict(NAND_PROBE)
    return state


@pytest.mark.parametrize('guarded', [True, False])
def test_unseeded_nand_reset_id_status_and_closed_gate(tmp_path, guarded):
    state = probe_state([
        ('write', 0x10, 0xFF), ('write', 0x10, 0x90),
        ('write', 8, 0), ('write', 0x20, 0),
        ('read', 0, 2), ('read', 0, 3),
        ('write', 0x10, 0x70), ('read', 0, 4), ('write', 0x24, 0)])
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded,
                      reset=True)
    assert result['stop']['reason'] == 'write_unmapped'
    assert result['registers'][2:5] == [0xEC, 0x35, 0xC0]
    assert result['nand']['reset_count'] == 1
    assert not result['nand']['read_open']
    assert result['nand']['spare_read']['known_bytes'] == 0
    assert result['nand']['spare_program']['operations'] == []
    assert 'main_array' not in result['nand']
    after = result['after_reset']['nand']
    assert after['reset_count'] == 0 and after['mode'] == 'unknown'
    assert after['spare_read']['known_bytes'] == 0


@pytest.mark.parametrize('guarded', [True, False])
@pytest.mark.parametrize('operations,reason', [
    ([('write', 0x10, 0x90)], 'nand_handoff_state_unmodeled'),
    ([('write', 0x10, 0xFF), ('write', 0x10, 0)],
     'nand_array_operation_unmodeled'),
    ([('write', 0x10, 0xFF), ('read', 0, 2)], 'nand_data_gate_closed'),
    ([('write', 0x10, 0xFF), ('write', 0x10, 0x90), ('write', 8, 0),
      ('write', 0x20, 0), ('read', 0, 2), ('read', 0, 3), ('read', 0, 4)],
     'nand_id_read_unmodeled'),
    ([('write', 0x10, 0xFF), ('write', 0x10, 0x50),
      ('write', 8, 0), ('write', 8, 0), ('write', 8, 0),
      ('write', 0x20, 0), ('read', 0, 2)], 'nand_read_unknown_spare'),
    ([('write', 0x10, 0xFF), ('write', 0x10, 0x50), ('write', 0x10, 0x80),
      ('write', 8, 0), ('write', 8, 0), ('write', 8, 0),
      ('write', 0, 0), ('write', 0x10, 0x10)], 'nand_program_unknown_spare')])
def test_unseeded_nand_retains_unknown_bytes_and_protocol_guards(
        tmp_path, guarded, operations, reason):
    state = probe_state(operations)
    if not guarded:
        state.pop('instruction_limit')
    result = qemu_run(state, tmp_path/'run', trace=False, research_guards=guarded)
    assert result['stop']['reason'] == reason
    assert result['nand']['spare_read']['known_bytes'] == 0
    assert result['nand']['spare_program']['operations'] == []
    assert 'main_array' not in result['nand']
