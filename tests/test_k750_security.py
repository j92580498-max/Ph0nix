"""Original K750 caller guards and the calibration setup read/modify/write."""
import pytest

from support.etx_fixtures import state as etx_state
from support.fixtures import base_state, qemu_run, words


K750_ETX_REQUEST = bytes.fromhex(
    '8ee639a56df194fda96feb86d082659ba4f4e6bd3c000d14abfd1a6153898cc82'
    '052d0c86207a6d7b69585e53271e14006f0265921d02d4d736cdc580b0b95a1f9'
    'fa521545905140f169f655cbcdaa485873')


@pytest.mark.parametrize('change', [None, 'caller', 'selection', 'request', 'opcode'])
def test_k750_etx_accepts_only_selected_original_request(tmp_path, change):
    state = etx_state()
    caller = 0x4493180c
    state['entry'] = caller
    state['executable_ranges'] = [{'start': caller, 'end': caller+4}]
    state['regions'][0]['address'] = caller & ~0xfff
    state['regions'][0]['seeds'][0]['address'] = caller
    state['regions'][1]['seeds'][0]['data'] = K750_ETX_REQUEST + bytes(66)
    state['synthetic_etx']['callers'] = 'k750-r1db001'
    state['synthetic_etx']['expected_request_hex'] = K750_ETX_REQUEST.hex()
    if change == 'caller':
        state['entry'] -= 2
        state['executable_ranges'][0]['start'] -= 2
        state['regions'][0]['seeds'][0]['address'] -= 2
    elif change == 'selection':
        state['synthetic_etx'].pop('callers')
    elif change == 'request':
        state['regions'][1]['seeds'][0]['data'] = bytes(148)
    elif change == 'opcode':
        state['regions'][0]['seeds'][0]['data'] = bytes.fromhex('9847fee7')
        state['registers'][3] = 0xffff0ddd
    result = qemu_run(state, tmp_path/'etx', trace=False)
    reason = ('synthetic_etx_payload_unmodeled' if change == 'request' else
              'synthetic_etx_caller_unmodeled' if change else 'instruction_limit')
    assert result['stop']['reason'] == reason
    assert result['synthetic_etx']['completed_calls'] == (0 if change else 1)
    assert result['synthetic_etx']['crypto_verification_established'] is False
    assert result['device_registers'][str(0xf9030004)] == (0 if change else 1)
    if change is None:
        assert result['registers'][1:14] == state['registers'][1:14]
        assert result['cpsr'] == state['cpsr']


@pytest.mark.parametrize('initial', [None, 0])
def test_original_k750_calibration_setup_uses_explicit_reset_input(tmp_path, initial):
    caller = 0x4482dda0
    state = base_state(words(0xeafffffe), entry=caller, cpsr=0xf3, limit=8)
    state['registers'][5] = 0xf50005d6
    state['registers'][6] = 8
    state['registers'][14] = 0x1000
    # Original LDRB, ORRS and STRB at 4482DDA0..4482DDA6; BX LR terminates
    # the isolated caller so the calibration tables need not be replaced.
    state['regions'].append({'name': 'fixture.original-k750-setup',
        'address': caller & ~0xfff, 'size': 4096, 'readonly': True,
        'seeds': [{'address': caller, 'data': bytes.fromhex('2978314329707047')}]})
    state['executable_ranges'].append({'start': caller, 'end': caller+8})
    state['f500_command_port'] = {'profile': 'word-ready-strobe-latency', 'latency_ns': 128}
    if initial is not None:
        state['f500_command_port']['setup_initial_value'] = initial
    result = qemu_run(state, tmp_path/'setup', trace=False, reset=True)
    assert result['stop']['reason'] == ('f500_port_read_unmodeled' if initial is None else 'instruction_limit')
    assert result['f500_command_port']['setup_value'] == (None if initial is None else 8)
    assert result['f500_command_port']['commands'] == 0
    assert result['after_reset']['f500_command_port'] == result['initial']['f500_command_port']
