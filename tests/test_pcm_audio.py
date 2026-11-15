"""Captured native PCM8 framing, sample precision and streaming continuity."""
from fractions import Fraction
import struct

import pytest

from test_audio_codec import codec


def oracle(samples):
    # Independent timeline calculation: 8kHz sample instants at a 48kHz
    # output clock, with the same documented linear presentation policy.
    scaled = [max(-32768, min(32767, value * 256)) for value in samples]
    result = []
    for tick in range(len(samples) * 6):
        before, remainder = divmod(tick, 6)
        if before == len(samples) - 1:
            value = Fraction(scaled[-1])
        else:
            value = Fraction(scaled[before] * (6 - remainder) + scaled[before + 1] * remainder, 6)
        rounded = (abs(value.numerator) * 2 + value.denominator) // (2 * value.denominator)
        result.append(rounded if value >= 0 else -rounded)
    return b''.join(struct.pack('<hh', value, value) for value in result)


@pytest.mark.parametrize('chunk', [1, 3, 7, 318, 500, 1280, 1588])
def test_native_expanded_pcm_has_audible_precision_and_fragment_independent_output(codec, chunk):
    # Includes both quantizer extrema and many 20ms/transport boundaries.
    samples = [((i * 37) % 256) - 127 for i in range(803)]
    raw = struct.pack('<' + 'h' * len(samples), *samples)
    result = codec(raw, 8000, 1, chunk=chunk, source_codec=3)
    assert result.returncode == 0, result.stderr
    assert result.stdout == oracle(samples)
    assert len(result.stdout) == len(samples) * 6 * 4
    assert b'\xff\x7f' in result.stdout


@pytest.mark.parametrize('samples', [[], [0], [-127], [128], [0, 1, -1]])
def test_short_and_empty_pcm_streams_do_not_invent_padding(codec, samples):
    raw = struct.pack('<' + 'h' * len(samples), *samples)
    result = codec(raw, 8000, 1, chunk=7, source_codec=3)
    assert result.returncode == 0, result.stderr
    assert result.stdout == oracle(samples)


@pytest.mark.parametrize('sample', [-32768, -128, 129, 32767])
def test_invalid_native_eight_bit_words_stop_without_output(codec, sample):
    result = codec(struct.pack('<h', sample), 8000, 1, source_codec=3)
    assert result.returncode == 2
    assert result.stderr.strip() == b'dsp_peer_pcm_source_sample_unmodeled'
    assert not result.stdout


def test_truncated_pcm_word_is_not_accepted(codec):
    result = codec(b'\x00', 8000, 1, source_codec=3)
    assert result.returncode == 2
    assert not result.stdout


@pytest.mark.parametrize('rate,channels', [(16000,1), (8000,2), (48000,2)])
def test_pcm_setup_must_match_the_recovered_native_contract(codec, rate, channels):
    result = codec(b'\0\0', rate, channels, source_codec=3)
    assert result.returncode == 2
    assert result.stderr.strip() == b'dsp_peer_audio_source_format_unmodeled'
