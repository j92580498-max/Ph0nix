"""Logical GDFS units retain their bytes in reconstructed native bank storage."""
import struct
import hashlib

import pytest

from db2010.preparation.gdfs import BANKS, BLOCK_SIZE, bank_image


def backup(*units):
    return struct.pack('<I', len(units)) + b''.join(
        struct.pack('<BHI', bank, unit, len(data)) + data for bank, unit, data in units)


def test_payloads_and_reverse_descriptors_survive_bank_reconstruction():
    units = [(0, 0x1234, bytes(range(17))), (2, 0x5678, b'\x00\xff\x80'),
             (0, 0x1235, b'\x01\x02')]
    raw, report = bank_image(backup(*units))
    assert len(raw) == 0x100000 and report['unit_count'] == 3
    assert report['bank_unit_counts'] == [2, 0, 1, 0, 0, 0, 0]
    assert report['payloads_preserved'] and not report['physical_dump']
    for bank in range(BANKS):
        assert raw[bank*BLOCK_SIZE:bank*BLOCK_SIZE+4] == struct.pack('<BBH', 0x1f, bank, 0)
    for bank, index, unit, offset, data in [(0, 1, 0x1234, 4, units[0][2]),
                                           (0, 2, 0x1235, 24, units[2][2]),
                                           (2, 1, 0x5678, 4, units[1][2])]:
        start = bank*BLOCK_SIZE
        descriptor = struct.unpack_from('<HHIII', raw, start+BLOCK_SIZE-16*index)
        assert descriptor == (0x3ff, unit, offset, len(data), 0xffffffff)
        assert raw[start+offset:start+offset+len(data)] == data
    assert raw[21:24] == b'\xff'*3
    assert raw[BANKS*BLOCK_SIZE:] == b'\xff'*BLOCK_SIZE


@pytest.mark.parametrize('raw,reason', [
    (b'\x01', 'size outside'),
    (struct.pack('<I', 0), 'unit count'),
    (struct.pack('<I', 1)+b'\x00', 'Truncated GDFS unit header'),
    (struct.pack('<I', 1)+struct.pack('<BHI', 0, 1, 2)+b'x', 'Truncated GDFS payload'),
    (backup((7, 1, b'x')), 'Invalid GDFS bank'),
    (backup((0, 1, b'')), 'Invalid GDFS bank or size'),
    (backup((0, 1, b'x'), (1, 1, b'y')), 'Duplicate GDFS unit'),
    (backup((0, 1, b'x'))+b'x', 'Trailing bytes'),
    (backup((0, 1, bytes(65536)), (0, 2, bytes(65520))), 'payload and index overlap'),
])
def test_malformed_backups_do_not_produce_a_bank(raw, reason):
    with pytest.raises(ValueError, match=reason):
        bank_image(raw)


def test_emulator_checksum_provisioning_changes_only_dynamic_prefixes():
    prefix = bytes(range(20))
    static = prefix + b'OTP12345' + bytes([5, 1, 5])
    bodies = {6: b'dynamic-one', 14: b'dynamic-two'}
    logical = backup((0, 19, static), (0, 6, b'a'*20+bodies[6]),
                     (0, 14, b'b'*20+bodies[14]), (2, 7, b'preserve-me'))
    original, _ = bank_image(logical)
    derived, report = bank_image(logical, emulator_dynamic_sha1=True)
    allowed = set()
    for index, unit in [(2, 6), (3, 14)]:
        _, actual, offset, size, _ = struct.unpack_from('<HHIII', derived, BLOCK_SIZE-16*index)
        assert actual == unit
        assert derived[offset:offset+20] == hashlib.sha1(prefix+bodies[unit]).digest()
        assert derived[offset+20:offset+size] == bodies[unit]
        allowed.update(range(offset, offset+20))
    assert {i for i, (a, b) in enumerate(zip(original, derived)) if a != b} <= allowed
    assert report['source_sha256'] == hashlib.sha256(logical).hexdigest()
    assert report['raw_sha256'] == hashlib.sha256(derived).hexdigest()
    assert not report['payloads_preserved']
    assert report['emulator_dynamic_checksums']['static_signature_preserved']
    assert report['emulator_dynamic_checksums']['KGEN_equivalence_established'] is False


@pytest.mark.parametrize('version,missing', [(5, True), (6, False)])
def test_checksum_provisioning_rejects_missing_units_and_dynamic3(version, missing):
    static = bytes(30)+bytes([version])
    units = [(0, 19, static), (0, 6, bytes(21))]
    if not missing:
        units.append((0, 14, bytes(21)))
    with pytest.raises(ValueError, match='Missing K750 security|dynamic3 checksum version'):
        bank_image(backup(*units), emulator_dynamic_sha1=True)
