"""Native AMR transport, malformed padding and active-audio checkpoint continuity."""
import copy
import json
import struct
import subprocess
import time

import pytest

from support.fa_wire10_fixtures import frame
from support.mma_playback_fixtures import make as playback_state, DELAY
from support.mma_ring_fixtures import stages, ACK, CLEAR, POLL
from support.mma_setup_fixtures import setup
from support.mma_mode_fixtures import mode
from test_amr_nb import amr
from test_audio_stream import packets
from test_native_checkpoint import engine, stopped, command, migration, comparable


# Complete captured K750 Super Real Tennis sound-effect serializer request.
SETUP = (0x0e00, 0, 0, 5, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
         0xffff, 0xffff, 0xffff, 0, 0, 0xffff, 3, 1, 0, 0, 0xcc03, 18)
RECEIVE = POLL[:20] + ACK + CLEAR


def access_units(count=8):
    # Synthetic MIME frames exercise both Tennis source rates without retaining
    # copyrighted game assets. Independent upstream decoding supplies the oracle.
    return [bytes([(6 + index % 2) << 3 | 4]) +
            bytes((37 * i + 71 * index) & 255
                  for i in range((27, 32)[index % 2] - 1))
            for index in range(count)]


def native_unit(data, index=0):
    assert len(data) % 2 == 0
    source = struct.unpack('>' + 'H' * (len(data) // 2), data)
    words = len(source) + 6
    timestamp = index * 20000
    return (frame([0x0804, words, 1, 1, 1, timestamp >> 16, timestamp & 65535,
                   len(source), *source, 20]) + POLL[:20] + CLEAR)


def queued(units, *, fragmented=False):
    steps = stages()['registered'] + setup(SETUP) + RECEIVE
    for index, unit in enumerate(units):
        # Every captured native Tennis unit is 17 source halfwords. FT7
        # contains 32 MIME bytes + 2 zeros; FT6 contains 27 + 7 zeros.
        data = unit + bytes(34 - len(unit))
        if fragmented:
            steps += native_unit(data[:8], index) + native_unit(data[8:], index)
        else:
            steps += native_unit(data, index)
    steps += frame([0x0804, 6, 2, 1, 1, 0, 0, 0, 20]) + POLL[:20] + CLEAR
    return steps + mode(0) + RECEIVE + RECEIVE


def prepared(steps):
    state = playback_state(steps, bytes(544))
    state['dsp_peer']['profile'] = 'wire20-modexp1024-mma-stream-peer'
    state['record_diagnostics'] = False
    return state


def finish(count):
    return (DELAY + RECEIVE) * (count + 1)


def oracle(amr, units):
    decoded = subprocess.run([amr], input=b''.join(units), capture_output=True)
    assert decoded.returncode == 0, decoded.stderr
    mono = struct.unpack('<' + 'h' * (len(decoded.stdout) // 2), decoded.stdout)
    output = bytearray()
    # Global 8kHz -> 48kHz sample positions with final-sample hold and
    # symmetric integer rounding; independent of production buffer boundaries.
    for index in range(len(mono) * 6):
        left, fraction = divmod(index, 6)
        right = min(left + 1, len(mono) - 1)
        weighted = mono[left] * (6 - fraction) + mono[right] * fraction
        sample = (abs(weighted) + 3) // 6
        if weighted < 0:
            sample = -sample
        output.extend(struct.pack('<hh', sample, sample))
    return bytes(output)


@pytest.mark.parametrize('fragmented', [False, True])
def test_native_ft6_ft7_stream_matches_independent_upstream_pcm(tmp_path, amr, fragmented):
    units = access_units()
    state = prepared(queued(units, fragmented=fragmented) + finish(len(units)))
    path = tmp_path / 'native.audio'
    with engine(state, tmp_path / 'native', audio=f'file,id=audio,path={path}') as vm:
        result = stopped(vm)
    assert result['stop']['reason'] == 'instruction_limit', result['stop']
    peer = result['dsp_peer']
    output = packets(path)
    assert peer['audio_source_codec'] == 'amr-nb'
    assert peer['audio_frames_decoded'] == len(units)
    assert peer['audio_frames_played'] == len(units) * 160 * 6
    assert peer['audio_completed'] and peer['audio_completions_acked'] == 1
    assert peer['audio_input_bytes'] == peer['ring4_buffered_words'] == 0
    assert peer['audio_credits_offered'] == peer['audio_credits_acked']
    assert b''.join(packet.pcm for packet in output) == oracle(amr, units)
    assert sum(packet.header['eos'] for packet in output) == 1


@pytest.mark.parametrize('codec,offset',
                         [(6, i) for i in range(27, 34)] + [(7, 32), (7, 33)])
def test_each_native_padding_byte_must_be_zero_before_decoding(tmp_path, codec, offset):
    unit = access_units(2)[codec - 6]
    data = bytearray(unit + bytes(34 - len(unit)))
    data[offset] = 1
    steps = (stages()['registered'] + setup(SETUP) + RECEIVE +
             native_unit(data) + mode(0) + RECEIVE + finish(1))
    with engine(prepared(steps), tmp_path / 'invalid') as vm:
        result = stopped(vm)
    assert result['stop']['reason'] == 'dsp_peer_amr_alignment_unmodeled'
    peer = result['dsp_peer']
    assert peer['audio_frames_decoded'] == peer['audio_frames_played'] == 0
    assert not peer['audio_amr_primed'] and not peer['audio_completed']


def test_missing_ft7_padding_is_not_consumed_as_a_following_frame(tmp_path):
    unit = access_units(2)[1]
    steps = (stages()['registered'] + setup(SETUP) + RECEIVE + native_unit(unit) +
             frame([0x0804, 6, 2, 1, 1, 0, 0, 0, 20]) + POLL[:20] + CLEAR +
             mode(0) + RECEIVE + finish(1))
    with engine(prepared(steps), tmp_path / 'truncated') as vm:
        result = stopped(vm)
    assert result['stop']['reason'] == 'dsp_peer_audio_truncated_source'
    assert result['dsp_peer']['audio_frames_decoded'] == 0


@pytest.mark.parametrize('fragmented', [False, True])
def test_active_amr_history_and_partial_pcm_survive_native_checkpoint(tmp_path, fragmented):
    units = access_units()
    start = queued(units, fragmented=fragmented)
    prefix = start + (DELAY + RECEIVE) * 2 + [('delay', 40000, 0, 0)]
    suffix = [('delay', 150000, 0, 0)] + RECEIVE + finish(len(units) - 3)
    state = prepared(prefix + suffix)
    first = copy.deepcopy(state)
    first['instruction_limit'] = prepared(prefix)['instruction_limit'] - 500
    checkpoint = tmp_path / 'active-amr.vmstate'
    full, beginning, resumed = [tmp_path / name for name in
                                ('full.audio', 'first.audio', 'last.audio')]
    with engine(state, tmp_path / 'whole', audio=f'file,id=audio,path={full}') as vm:
        whole = stopped(vm)
    with engine(first, tmp_path / 'first', audio=f'file,id=audio,path={beginning}') as vm:
        before = stopped(vm)
        peer = before['dsp_peer']
        assert peer['audio_active'] and peer['audio_amr_primed']
        assert peer['audio_frames_decoded'] >= 3
        assert peer['audio_pending_pcm_frames'] > 0
        assert 0 < peer['audio_sample_emitted'] < peer['audio_pending_pcm_frames']
        layout = json.loads(command(vm, 'qom-get', {
            'path': '/machine', 'property': 'checkpoint-layout'}))
        migration(vm, 'migrate', checkpoint)
    continuation = copy.deepcopy(state)
    continuation['instruction_limit'] -= first['instruction_limit']
    continuation['checkpoint_tcm'] = layout
    with engine(continuation, tmp_path / 'restored', incoming=True,
                audio=f'file,id=audio,path={resumed}') as vm:
        migration(vm, 'migrate-incoming', checkpoint)
        assert vm.snapshot(time.monotonic() + 5)['dsp_peer'] == before['dsp_peer']
        after = stopped(vm)
    assert comparable(after) == comparable(whole)
    complete = packets(full)
    joined = packets(beginning) + packets(resumed)
    assert b''.join(packet.pcm for packet in joined) == b''.join(packet.pcm for packet in complete)
    assert [(p.header['first_frame'], p.header['frame_count']) for p in joined] == [
        (p.header['first_frame'], p.header['frame_count']) for p in complete]
    assert sum(packet.header['eos'] for packet in joined) == 1
    assert after['dsp_peer']['audio_frames_played'] == len(units) * 160 * 6
