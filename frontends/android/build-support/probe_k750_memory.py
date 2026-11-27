"""Report metadata from the disposable K750i file-transfer test."""
import hashlib
from pathlib import Path
import struct
import sys

memory = Path(sys.argv[1]).read_bytes()
assert memory.startswith(b'K750MEM\1')
fs_start = 184 + 0x13e0000
nor = memory[fs_start:fs_start + 0xb00000]
nand = memory[fs_start + 0xc00000:]
main, spare = nand[:0x2000000], nand[0x2000000:]
page_count = len(main) // 512
erased = b'\xff' * 512
print('probe payload page:', main.find(b'ph0nix file transfer remains usable') // 512)
for n in range(page_count):
    data, oob = main[n*512:(n+1)*512], spare[n*16:(n+1)*16]
    if data == erased and oob == b'\xff' * 16:
        continue
    kind = '-'
    if data[3:11] == b'MSWIN4.1':
        kind = 'boot:' + data[11:36].hex()
    elif n < 192 or b'ph0nix file transfer remains usable' in data or b'RESUME' in data:
        kind = 'sample:' + data[:24].hex()
    if b'ph0nix file transfer remains usable' in data:
        kind = 'payload:' + kind
    print(f'ftl {n:04x} {oob.hex()} {"-" if data == erased else hashlib.sha256(data).hexdigest()[:16]} {kind}')
for span in range(0, len(nor), 0x40000):
    tags = [struct.unpack_from('<I', nor, span+i*1028+1024)[0] for i in range(255)]
    print(f'nor {span//0x40000} {nor[span+0x3fff8:span+0x40000].hex()} '
          + ' '.join(f'{tag:08x}' for tag in tags))
