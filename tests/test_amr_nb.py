"""Canonical fixed-point history must equal uninterrupted upstream decoding."""
from pathlib import Path
import subprocess

import pytest

from db2010.paths import ROOT


@pytest.fixture(scope='module')
def amr(tmp_path_factory):
    output = tmp_path_factory.mktemp('amr') / 'compare'
    vendor = ROOT / 'qemu/hw/arm/db2010-opencore-amrnb'
    includes = [ROOT / 'qemu/hw/arm', vendor / 'oscl',
                vendor / 'opencore/codecs_v2/audio/gsm_amr/amr_nb/dec/src',
                vendor / 'opencore/codecs_v2/audio/gsm_amr/amr_nb/dec/include',
                vendor / 'opencore/codecs_v2/audio/gsm_amr/amr_nb/common/include',
                vendor / 'opencore/codecs_v2/audio/gsm_amr/common/dec/include']
    compiled = subprocess.run(['cc', '-std=c11', '-O2', *['-I'+str(p) for p in includes],
                    str(Path(__file__).parent / 'support/amr_nb.c'),
                    str(ROOT / 'qemu/build/hw/arm/db2010-opencore-amrnb/libdb2010-amrnb.a'),
                    '-o', str(output)], capture_output=True)
    assert compiled.returncode == 0, compiled.stderr.decode()
    return output


def frames(modes, count=12):
    lengths = [13, 14, 16, 18, 20, 21, 27, 32, 6]
    return b''.join(bytes([mode << 3 | 4]) +
                    bytes((37*i + 11*mode + 13*n) & 255 for i in range(lengths[mode]-1))
                    for n in range(count) for mode in modes)


@pytest.mark.parametrize('mode', range(8))
def test_every_amr_nb_speech_rate_matches_persistent_upstream_state(amr, mode):
    result = subprocess.run([amr], input=frames([mode]), capture_output=True)
    assert result.returncode == 0, result.stderr
    assert result.stderr == b'frames=12\n'
    assert len(result.stdout) == 12 * 160 * 2
    assert any(result.stdout)


def test_rate_changes_sid_and_missing_frames_preserve_all_history(amr):
    data = frames(range(9), count=8) + b'\x7c' * 12 + frames([0, 7], count=8)
    result = subprocess.run([amr], input=data, capture_output=True)
    assert result.returncode == 0, result.stderr
    assert len(result.stdout) == (72 + 12 + 16) * 160 * 2


def test_bad_frame_quality_uses_upstream_concealment_policy(amr):
    data = bytearray(frames([7], count=10))
    for index in range(3, 7):
        data[index * 32] &= ~4
    result = subprocess.run([amr], input=data, capture_output=True)
    assert result.returncode == 0, result.stderr


def test_invalid_serialized_history_is_refused_without_changing_state(amr):
    result = subprocess.run([amr, 'tamper'], input=frames(range(8), count=2), capture_output=True)
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize('data', [b'\x80', b'\x05'+bytes(12), b'\x4c', b'\x04'+bytes(11)])
def test_malformed_and_truncated_access_units_are_refused(amr, data):
    result = subprocess.run([amr], input=data, capture_output=True)
    assert result.returncode == 2
    assert result.stderr == b'invalid-frame\n'
    assert not result.stdout
