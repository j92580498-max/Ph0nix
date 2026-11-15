"""Independent pitch, envelope, voice ownership and packet-boundary checks."""
from array import array
from pathlib import Path
import subprocess

import pytest
from db2010.paths import ROOT


@pytest.fixture(scope='module')
def synth(tmp_path_factory):
    tmp = tmp_path_factory.mktemp('midi-synth')
    # Only the module's QEMU standard-header umbrella is replaced for this
    # host-only harness; compile the unchanged production source.
    (tmp/'qemu').mkdir()
    (tmp/'qemu/osdep.h').write_text('#include <stdint.h>\n#include <stdbool.h>\n#include <stddef.h>\n')
    binary = tmp/'synth'
    subprocess.run(['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror',
        '-I'+str(tmp), '-I'+str(ROOT/'qemu/hw/arm'),
        str(ROOT/'qemu/hw/arm/db2010-midi-synth.c'),
        str(Path(__file__).parent/'support/midi_synth.c'), '-o', str(binary)],
        check=True, capture_output=True)
    def run(script, ok=True, stereo=False):
        result = subprocess.run([binary], input=script.encode(), capture_output=True)
        if ok:
            assert result.returncode == 0, result.stderr
        pcm = array('h', result.stdout)
        if not stereo:
            assert pcm[::2] == pcm[1::2]
        return pcm if stereo else pcm[::2], result
    return run


def test_a4_pitch_velocity_and_fixed_headroom(synth):
    loud, _ = synth('e 144 69 127\nr 48000\n')
    soft, _ = synth('e 144 69 63\nr 48000\n')
    crossings = sum(a <= 0 < b for a, b in zip(loud, loud[1:]))
    assert crossings == 440
    assert 810 <= max(loud) <= 820
    assert 0.49 < max(soft)/max(loud) < 0.51


def test_release_ends_at_64ms_and_no_noteoff_cross_channel(synth):
    pcm, _ = synth('e 144 60 127\nr 100\ne 145 60 0\nr 100\ne 128 60 0\nr 3072\nr 100\n')
    assert any(pcm[100:200])
    assert any(pcm[200:2200])
    assert not any(pcm[-100:])


def test_same_key_releases_oldest_held_voice(synth):
    _, result = synth('e 144 60 100\ne 144 60 120\ne 144 60 0\ns\n')
    assert result.stderr == b'2 0 0:0:60:0 1:0:60:1\n'


def test_steal_releasing_before_oldest_held(synth):
    script = ''.join(f'e 144 {i} 127\n' for i in range(40))
    _, result = synth(script+'e 128 35 0\ne 145 99 100\ns\ne 145 98 100\ns\n')
    lines = result.stderr.decode().splitlines()
    assert lines[0].startswith('41 1 0:0:0:1 ')
    assert '35:1:99:1' in lines[0]
    assert lines[1].startswith('42 2 0:1:98:1 ')


def test_packet_boundaries_and_programs_do_not_change_triangle(synth):
    whole, _ = synth('e 153 69 127\nr 5000\n')
    split, _ = synth('e 201 81 0\ne 153 69 127\nr 17\nr 1000\nr 3983\n')
    assert whole == split


def test_native_k750_startup_bank_selection_keeps_declared_triangle(synth):
    whole, _ = synth('e 145 69 127\nr 5000\n')
    selected, _ = synth('e 177 0 122\ne 177 32 0\ne 193 12 0\ne 145 69 127\nr 5000\n')
    assert selected == whole


def test_bank_halves_preserve_each_other_and_channel_selection(synth):
    _, result = synth('e 177 32 17\ne 177 0 122\nb 1\ne 177 32 5\nb 1\ne 177 0 3\nb 1\nb 0\n')
    assert result.stderr == b'15633\n15621\n389\n0\n'


def test_channel_volume_and_two_semitone_bend(synth):
    silent, _ = synth('e 176 7 0\ne 144 69 127\nr 48000\n')
    assert not any(silent)
    bent, _ = synth('e 224 127 127\ne 144 69 127\nr 48000\n')
    crossings = sum(a <= 0 < b for a, b in zip(bent, bent[1:]))
    assert 493 <= crossings <= 494


def test_expression_scales_volume_and_can_mute_existing_notes(synth):
    full, _ = synth('e 144 69 127\nr 4800\n')
    soft, _ = synth('e 176 11 52\ne 144 69 127\nr 4800\n')
    assert 0.40 < max(soft)/max(full) < 0.42
    muted, _ = synth('e 144 69 127\nr 100\ne 176 11 0\nr 100\n')
    assert any(muted[:100]) and not any(muted[100:])


def test_pan_has_independent_stereo_channels_and_preserves_center(synth):
    center, _ = synth('e 176 10 64\ne 144 69 127\nr 4800\n')
    left, _ = synth('e 176 10 0\ne 144 69 127\nr 4800\n', stereo=True)
    right, _ = synth('e 176 10 127\ne 144 69 127\nr 4800\n', stereo=True)
    assert left[::2] == right[1::2] == center
    assert not any(left[1::2]) and not any(right[::2])


def test_modulation_is_audible_and_packet_boundaries_preserve_its_phase(synth):
    plain, _ = synth('e 144 69 127\nr 9600\n')
    whole, _ = synth('e 176 1 127\ne 144 69 127\nr 9600\n')
    split, _ = synth('e 176 1 127\ne 144 69 127\nr 17\nr 9583\n')
    assert whole == split and whole != plain


def test_all_notes_off_releases_only_the_selected_channel(synth):
    _, result = synth('e 144 60 100\ne 145 64 100\ne 176 123 0\ns\ne 177 120 0\ns\n')
    assert result.stderr == b'2 0 0:0:60:0 1:1:64:1\n2 0 0:0:60:0\n'


@pytest.mark.parametrize('event', ['176 64 100', '208 64 0', '240 0 0', '144 128 1'])
def test_unimplemented_controls_are_guarded(synth, event):
    _, result = synth(f'e {event}\n', ok=False)
    assert result.returncode == 2
    assert result.stderr == b'dsp_peer_midi_event_unmodeled\n'
