"""Exercise the timed MIDI renderer with an optional private SoundFont."""
import os
from pathlib import Path
import subprocess

import pytest

from db2010.paths import ROOT


def test_soundfont_timed_stream(tmp_path):
    include = tmp_path / 'qemu'
    include.mkdir()
    (include / 'osdep.h').write_text(
        '#include <stdint.h>\n#include <stdbool.h>\n#include <inttypes.h>\n'
        '#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n'
        '#define MIN(left, right) ((left) < (right) ? (left) : (right))\n'
    )
    arm = ROOT / 'qemu/hw/arm'
    harness = Path(__file__).parent / 'support/midi_soundfont.c'

    def compile_renderer(name, flags=()):
        binary = tmp_path / name
        subprocess.run(['cc', '-std=c11', '-O2', '-I', str(tmp_path), '-I', str(arm),
                        *flags, str(arm / 'db2010-midi-synth.c'),
                        str(arm / 'db2010-midi-stream.c'), str(harness), '-lm', '-o', str(binary)],
                       check=True)
        return binary

    environment = os.environ.copy()
    bank = environment.pop('PH0NIX_TEST_MIDI_SOUNDFONT', None)
    header = environment.pop('PH0NIX_TEST_TSF_INCLUDE', None)
    environment.pop('PH0NIX_MIDI_SOUNDFONT', None)
    triangle = subprocess.check_output([compile_renderer('triangle')], env=environment)
    if not (bank and header):
        pytest.skip('Set PH0NIX_TEST_MIDI_SOUNDFONT and PH0NIX_TEST_TSF_INCLUDE for the private bank')
    renderer = compile_renderer('soundfont', ['-DCONFIG_DB2010_SOUNDFONT', '-isystem', header])
    assert subprocess.check_output([renderer], env=environment) == triangle
    assert subprocess.check_output([renderer], env={**environment, 'PH0NIX_MIDI_SOUNDFONT': str(tmp_path / 'missing.sf2')}) == triangle
    sampled = subprocess.check_output([renderer, 'soundfont'], env={**environment, 'PH0NIX_MIDI_SOUNDFONT': bank})
    assert sampled != triangle
