"""Reconstruct seven GDFS v0 NOR banks from a logical unit backup.

The backup stores a LE32 count followed by bank:u8, unit:LE16, size:LE32
and payload bytes. R1DB001's native GDFS driver establishes the bank header
and reverse descriptor layout. Payloads stay exact; storage metadata is an
explicit compatibility input, not a physical flash dump.
"""
import hashlib
import struct


GDFS_BASE = 0x45f00000
GDFS_SIZE = 0x100000
BLOCK_SIZE = 0x20000
BANKS = 7


def bank_image(backup, *, emulator_dynamic_sha1=False):
    """Keep original units, optionally deriving two emulator-only checksums.

    R1DB001 hashes the static signature prefix followed by each dynamic body.
    Our mode4 hardware substitute uses SHA-1; this optional provisioning input
    does not establish KGEN equivalence or validate the static signature.
    """
    if not 4 <= len(backup) <= GDFS_SIZE:
        raise ValueError('GDFS backup size outside supported bounds')
    count, = struct.unpack_from('<I', backup)
    if not 1 <= count <= BANKS * ((BLOCK_SIZE - 4) // 20):
        raise ValueError('Invalid GDFS unit count')
    image = bytearray(b'\xff' * GDFS_SIZE)
    cursors, counts, payload_sizes = [4] * BANKS, [0] * BANKS, [0] * BANKS
    for bank in range(BANKS):
        struct.pack_into('<BBH', image, bank * BLOCK_SIZE, 0x1f, bank, 0)
    position, seen, security_units = 4, set(), {}
    for index in range(count):
        if position + 7 > len(backup):
            raise ValueError(f'Truncated GDFS unit header at {index}')
        bank, unit, size = struct.unpack_from('<BHI', backup, position)
        position += 7
        if bank >= BANKS or not 0 < size <= BLOCK_SIZE - 20:
            raise ValueError(f'Invalid GDFS bank or size at unit {index}')
        if unit in seen:
            raise ValueError(f'Duplicate GDFS unit at {index}')
        if position + size > len(backup):
            raise ValueError(f'Truncated GDFS payload at unit {index}')
        payload_offset = cursors[bank]
        next_offset = (payload_offset + size + 3) & ~3
        descriptor_offset = BLOCK_SIZE - 16 * (counts[bank] + 1)
        if next_offset > descriptor_offset:
            raise ValueError(f'GDFS payload and index overlap in bank {bank}')
        start = bank * BLOCK_SIZE
        image[start + payload_offset:start + payload_offset + size] = backup[position:position + size]
        if unit in (6, 14, 19):
            security_units[unit] = (bank, start + payload_offset, size)
        struct.pack_into('<HHIII', image, start + descriptor_offset,
                         0x3ff, unit, payload_offset, size, 0xffffffff)
        counts[bank] += 1
        payload_sizes[bank] += size
        cursors[bank] = next_offset
        seen.add(unit)
        position += size
    if position != len(backup):
        raise ValueError('Trailing bytes after GDFS units')
    changes = []
    if emulator_dynamic_sha1:
        if (set(security_units) != {6, 14, 19}
                or any(bank != 0 or size < 20 for bank, _, size in security_units.values())
                or security_units[19][2] < 31):
            raise ValueError('Missing K750 security units for emulator checksum provisioning')
        static_offset = security_units[19][1]
        if image[static_offset + 30] > 5:
            raise ValueError('Unsupported K750 dynamic3 checksum version')
        prefix = image[static_offset:static_offset + 20]
        for unit in (6, 14):
            bank, offset, size = security_units[unit]
            original = bytes(image[offset:offset + 20])
            derived = hashlib.sha1(prefix + image[offset + 20:offset + size]).digest()
            image[offset:offset + 20] = derived
            changes.append({'bank': bank, 'unit': unit, 'offset': 0, 'size_bytes': 20,
                            'original_hex': original.hex(), 'derived_hex': derived.hex(),
                            'body_preserved': True})
    image = bytes(image)
    return image, {
        'profile': 'logical-gdfs-v0-active-banks',
        'source_bytes': len(backup), 'source_sha256': hashlib.sha256(backup).hexdigest(),
        'raw_bytes': len(image), 'raw_sha256': hashlib.sha256(image).hexdigest(),
        'address': GDFS_BASE, 'unit_count': count,
        'bank_unit_counts': counts, 'bank_payload_bytes': payload_sizes,
        'payloads_preserved': not changes, 'physical_dump': False,
        **({'emulator_dynamic_checksums': {
            'profile': 'sha1-static-prefix-dynamic6-14', 'changes': changes,
            'KGEN_equivalence_established': False, 'static_signature_preserved': True,
        }} if changes else {}),
    }
