"""Native hash FIFO results against hashlib, including interrupted transfers."""
import copy
import hashlib
import struct

import pytest

from support.fixtures import base_state, qemu_run, words
from test_native_checkpoint import comparable, engine, migration, stopped


BASE = 0xf9030000
OUT = 0x4c000000


def write(offset, value, width=4):
    return ('w', BASE + offset, value, width)


def fixture(steps):
    code, literals, patches = [], [], []
    reads = 0
    for op, address, value, width in steps:
        for register, literal in ((0, value), (1, address)):
            patches.append((len(code), register, len(literals)))
            code.append(0)
            literals.append(literal)
        code.append({('w', 4): 0xe5810000, ('w', 2): 0xe1c100b0,
                     ('r', 4): 0xe5912000}[(op, width)])
        if op == 'r':
            code.append(0xe5832000 | (4 * reads))
            reads += 1
    code += [0xe89301f0, 0xeafffffe]  # Read the five output words into R4..R8.
    for index, register, literal in patches:
        offset = 4 * (len(code) + literal - index) - 8
        assert 0 <= offset < 4096
        code[index] = 0xe59f0000 | register << 12 | offset
    state = base_state(words(*code, *literals), limit=len(code) + 1)
    state['hash_accelerator'] = {'profile': 'sha1-fifo-immediate-8040'}
    state['registers'][3] = OUT
    state['regions'].append({'name': 'fixture.output', 'address': OUT, 'size': 4096,
                            'seeds': [{'address': OUT, 'data': bytes(20)}]})
    state['record_diagnostics'] = False
    return state


def message_steps(data, mode=3):
    padded = data + b'\x80' + bytes((55 - len(data)) % 64) + struct.pack('>Q', len(data) * 8)
    steps = [write(0x0c, mode), write(0x10, 0x80408040)]
    for block in range(0, len(padded), 64):
        steps.append(write(0, 1 if block == 0 else 8))
        steps.extend(write(0x14, value) for value in struct.unpack('<16I', padded[block:block + 64]))
    steps.extend(('r', BASE + offset, 0, 4) for offset in range(0x18, 0x2c, 4))
    return steps


@pytest.mark.parametrize('length', [0, 3, 55, 56, 64, 129])
@pytest.mark.parametrize('mode', [3, 4])
def test_native_hash_matches_sha1_arithmetic_assumption(tmp_path, length, mode):
    data = bytes((i * 37 + 11) % 256 for i in range(length))
    result = qemu_run(fixture(message_steps(data, mode)), tmp_path/'hash', trace=False, reset=True)
    assert result['stop']['reason'] == 'instruction_limit'
    assert struct.pack('<5I', *result['registers'][4:9]) == hashlib.sha1(data).digest()
    assert result['hash_accelerator']['blocks'] == (length + 9 + 63) // 64
    assert result['hash_accelerator']['KGEN_equivalence_established'] is False
    assert result['after_reset']['hash_accelerator'] == result['initial']['hash_accelerator']


@pytest.mark.parametrize('steps, reason', [
    ([write(0, 1)], 'hash_mode_unselected'),
    ([write(0x0c, 2)], 'hash_mode_unmodeled'),
    ([write(0x0c, 3), write(0, 8)], 'hash_state_unknown'),
    ([write(0x0c, 3), write(0x14, 0)], 'hash_fifo_not_ready'),
    ([write(0x0c, 3), write(0, 1), write(0, 8)], 'hash_fifo_incomplete'),
    ([write(0x0c, 3), write(0, 1, 2)], 'hash_access_unmodeled'),
])
def test_native_hash_rejects_unestablished_sequence(tmp_path, steps, reason):
    result = qemu_run(fixture(steps), tmp_path/'guard', trace=False)
    assert result['stop']['reason'] == reason
    assert result['hash_accelerator']['blocks'] == 0


def test_native_hash_fifo_migration_preserves_partial_block(tmp_path):
    state = fixture(message_steps(b'abc'))
    with engine(state, tmp_path/'whole') as vm:
        whole = stopped(vm)
    first = copy.deepcopy(state)
    first['instruction_limit'] = 3 * 8  # Mode, seed, start and five FIFO words.
    checkpoint = tmp_path/'hash.vmstate'
    with engine(first, tmp_path/'prefix') as vm:
        prefix = stopped(vm)
        assert prefix['hash_accelerator']['input_active']
        assert prefix['hash_accelerator']['fifo_words'] == 5
        migration(vm, 'migrate', checkpoint)
    resumed = copy.deepcopy(state)
    resumed['instruction_limit'] -= first['instruction_limit']
    with engine(resumed, tmp_path/'restored', incoming=True) as vm:
        migration(vm, 'migrate-incoming', checkpoint)
        final = stopped(vm)
    assert comparable(final) == comparable(whole)
    assert struct.pack('<5I', *final['registers'][4:9]) == hashlib.sha1(b'abc').digest()


@pytest.mark.parametrize('sha, caller, target', [
    (True, 0x44da9b28, 0xffff0f49), (False, 0x44da9b96, 0xffff12a3),
])
@pytest.mark.parametrize('bad_caller', [False, True])
def test_k750_hash_init_uses_its_original_rom_call_site(tmp_path, sha, caller, target, bad_caller):
    from support.kgen_candidate import initialize
    from support.kgen_fixtures import context_from, state as init_state
    state = init_state('init')
    state.pop('synthetic_kgen')
    key = 'synthetic_sha' if sha else 'synthetic_kgen'
    state[key] = {'profile': 'sha1-le92-init-update-final-8040', 'callers': 'k750-r1db001'}
    state['entry'] = caller - (2 if bad_caller else 0)
    state['registers'][1] = target
    state['executable_ranges'] = [{'start': state['entry'], 'end': state['entry'] + 4}]
    state['regions'][0]['address'] = caller & ~0xfff
    state['regions'][0]['seeds'][0]['address'] = state['entry']
    result = qemu_run(state, tmp_path/'init', trace=False, inspect='xp /23wx 0x4c000000')
    assert result['stop']['reason'] == (key + '_caller_unmodeled' if bad_caller else 'instruction_limit')
    assert result[key]['init_calls'] == (0 if bad_caller else 1)
    if not bad_caller:
        assert context_from(result) == initialize()
