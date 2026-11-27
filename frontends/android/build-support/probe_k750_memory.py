"""Analyze the saved internal flash of the disposable K750i firmware test."""
from collections import Counter
from pathlib import Path
import struct
import sys

from db2010.preparation.filesystem import Fat16

memory = Path(sys.argv[1]).read_bytes()
assert memory.startswith(b'K750MEM\1')
fs_start = 184 + 0x13e0000
nor = memory[fs_start:fs_start + 0xb00000]
nand = memory[fs_start + 0xc00000:]
main, spare = nand[:0x2000000], nand[0x2000000:]
print('k750i_offline_flash:', dict(
    nor_file=nor.find(b'ph0nix file transfer remains usable'),
    nand_file=main.find(b'ph0nix file transfer remains usable'),
    nand_pages=sum(main[i:i+512] != b'\xff'*512 for i in range(0,len(main),512)),
    nand_spare=Counter(spare[i:i+16].hex() for i in range(0,len(spare),16)).most_common(12),
    nor_tags=Counter(nor[i+j*1028+1027] for i in range(0,len(nor),0x40000) for j in range(255)),
    nor_markers=[hex(i+j*1028) for i in range(0,len(nor),0x40000) for j in range(255)
                 if nor[i+j*1028+1024:i+j*1028+1028] == b'\x54\x12\xcd\xab'],
    nand_boot=[i for i in range(0,len(main),512) if main[i+3:i+11] == b'MSWIN4.1'],
))
try:
    disk = Fat16(nor)
    user = disk.find('tpa/user')
    print('k750i_offline_nor:', [(e['name'], e['cluster']) for e in disk.entries(user['cluster'])])
    for path in ['image', 'other', 'audio', 'video', 'theme']:
        entry = disk.find('tpa/user/'+path)
        if entry:
            try:
                print(path, [e['name'] for e in disk.entries(entry['cluster'])][:30])
            except ValueError as e:
                print(path, 'unavailable:', e)
except (ValueError, UnicodeError) as e:
    print('k750i_offline_nor_unavailable:', e)
