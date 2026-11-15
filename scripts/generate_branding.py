#!/usr/bin/env python3
"""Export the original button-phone pixel grid without image dependencies.

Run from any directory: python3 scripts/generate_branding.py
SVG, Android vectors and PNGs all use the same grid and flat palette.
"""
import json
from html import escape
from pathlib import Path
import struct
import zlib

ROOT = Path(__file__).resolve().parents[1]
BRANDING = ROOT / 'assets/branding'
ANDROID = ROOT / 'frontends/android/app/src/main/res'


def png(pixels, palette, scale):
    size = len(pixels)
    def chunk(kind, data):
        return (struct.pack('>I', len(data)) + kind + data +
                struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff))
    rows = bytearray()
    for row in pixels:
        rgba = b''.join(palette.get(cell, bytes(4)) * scale for cell in row)
        rows.extend((b'\0' + rgba) * scale)
    return (b'\x89PNG\r\n\x1a\n' +
            chunk(b'IHDR', struct.pack('>IIBBBBB', size * scale, size * scale, 8, 6, 0, 0, 0)) +
            chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b''))


def main():
    spec = json.loads((BRANDING / 'logo.pixels.json').read_text())
    size, pixels, colors = spec['size'], spec['pixels'], spec['palette']
    assert len(pixels) == size and all(len(row) == size for row in pixels)
    assert all(cell == '.' or cell in colors for row in pixels for cell in row)
    palette = {key: bytes.fromhex(color[1:]) + b'\xff' for key, color in colors.items()}
    paths = {key: [] for key in colors}
    for y, row in enumerate(pixels):
        x = 0
        while x < size:
            cell, end = row[x], x + 1
            while end < size and row[end] == cell:
                end += 1
            if cell != '.':
                width = end - x
                paths[cell].append(f'M{x},{y}h{width}v1h-{width}z')
            x = end
    svg_paths = '\n'.join(f'  <path fill="{colors[key]}" d="{"".join(runs)}"/>'
                          for key, runs in paths.items())
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" '
           f'viewBox="0 0 {size} {size}" shape-rendering="crispEdges" role="img">\n'
           f'  <title>{escape(spec["name"])}</title>\n'
           f'  <desc>{escape(spec["description"])}</desc>\n'
           f'{svg_paths}\n</svg>\n')
    (BRANDING / 'logo.svg').write_text(svg)
    (BRANDING / 'logo.png').write_bytes(png(pixels, palette, 512 // size))
    icon = png(pixels, palette, 64 // size)
    (BRANDING / 'logo-icon.png').write_bytes(icon)
    browser = ROOT / 'src/db2010/ui/assets'
    (browser / 'logo.svg').write_text(svg)
    (browser / 'logo-icon.png').write_bytes(icon)
    vector_paths = '\n'.join(f'    <path android:fillColor="{colors[key]}" '
                              f'android:pathData="{"".join(runs)}"/>'
                              for key, runs in paths.items())
    vector = ('<?xml version="1.0" encoding="utf-8"?>\n'
              '<!-- Generated from assets/branding/logo.pixels.json. -->\n'
              '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
              f'    android:width="108dp" android:height="108dp"\n'
              f'    android:viewportWidth="{size}" android:viewportHeight="{size}">\n'
              f'{vector_paths}\n</vector>\n')
    (ANDROID / 'drawable/ic_logo.xml').write_text(vector.replace('108dp', '32dp'))
    # Keep the complete handset within the adaptive launcher's mask.
    margin = 12
    foreground = vector.replace(f'android:viewportWidth="{size}" android:viewportHeight="{size}"',
        f'android:viewportWidth="{size + 2 * margin}" android:viewportHeight="{size + 2 * margin}"')
    foreground = foreground.replace(vector_paths,
        f'    <group android:translateX="{margin}" android:translateY="{margin}">\n' + vector_paths + '\n    </group>')
    (ANDROID / 'drawable/ic_launcher_foreground.xml').write_text(foreground)
    legacy = ANDROID / 'mipmap-anydpi'
    legacy.mkdir(exist_ok=True)
    (legacy / 'ic_launcher.xml').write_text(vector)
    monochrome = foreground.replace(
        f'    <group android:translateX="{margin}" android:translateY="{margin}">\n' + vector_paths + '\n    </group>',
        f'    <group android:translateX="{margin}" android:translateY="{margin}">\n' +
        '    <path android:fillColor="#FFFFFF" android:pathData="' +
        ''.join(run for key, runs in paths.items() if key not in spec['monochrome_cutouts'] for run in runs) + '"/>\n    </group>')
    (ANDROID / 'drawable/ic_logo_monochrome.xml').write_text(monochrome)
    print('Exported button-phone SVG, transparent PNGs and Android vectors.')


if __name__ == '__main__':
    main()
