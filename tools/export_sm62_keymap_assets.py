#!/usr/bin/env python3
"""Export uncompressed LVGL Zen glyphs and the firmware's 4x4 AA grid mask.
Read-only firmware input; no substitute host font or hand-tuned outline.
Usage: python3 tools/export_sm62_keymap_assets.py PATH_TO_FIRMWARE
"""
import json
import re
import sys
from pathlib import Path
from PIL import Image

root = Path(sys.argv[1])
out = Path(__file__).resolve().parents[1] / 'assets/sm62'
metadata = []
for size in (28, 20):
    source = root / f'keyboards/ergohaven/macropad/sm62_font_zen_medium_{size}.c'
    text = source.read_text()
    raw = re.search(r'glyph_bitmap\[\] = \{(.*?)\n\};', text, re.S)[1]
    raw = re.sub(r'/\*.*?\*/', '', raw, flags=re.S)
    bitmap = bytes(int(v, 16) for v in re.findall(r'0x[0-9a-fA-F]+', raw))
    descriptors = re.search(r'glyph_dsc\[\] = \{(.*?)\n\};', text, re.S)[1]
    entries = [dict((k, int(v)) for k, v in re.findall(r'\.(\w+)\s*=\s*(-?\d+)', entry))
               for entry in re.findall(r'\{([^{}]+)\}', descriptors)]
    line = int(re.search(r'\.line_height = (\d+)', text)[1])
    baseline = int(re.search(r'\.base_line = (\d+)', text)[1])
    image = Image.new('RGBA', (32 * 16, 40 * 11))
    glyphs = []
    cmap = re.search(r'cmaps\[\].*?=\s*\{(.*?)\n\};', text, re.S)[1]
    mapping = []
    for start, length, first_id in re.findall(r'\.range_start = (\d+), \.range_length = (\d+), \.glyph_id_start = (\d+)', cmap):
        mapping.extend((int(start) + i, int(first_id) + i) for i in range(int(length)))
    for slot, (cp, glyph_id) in enumerate(mapping):
        d = entries[glyph_id]
        x, y = slot % 16 * 32, slot // 16 * 40
        w, h = d['box_w'], d['box_h']
        for p in range(w * h):
            b = bitmap[d['bitmap_index'] + p // 2]
            alpha = ((b >> 4) if p % 2 == 0 else (b & 15)) * 17
            image.putpixel((x + p % w, y + p // w), (255, 255, 255, alpha))
        glyphs.append([cp, slot, w, h, d['ofs_x'], line - baseline - h - d['ofs_y'], (d['adv_w'] + 8) // 16])
    image.save(out / f'keymap-zen-{size}.png')
    metadata.append({'size': size, 'glyphs': glyphs})
(out / 'keymap-glyphs.json').write_text(json.dumps(metadata, separators=(',', ':')) + '\n')
# Exact init_grid_junction_alpha() from screen_keymap.c: A8 coverage.
image = Image.new('RGBA', (20, 20))
for y in range(20):
    for x in range(20):
        coverage = 16 if x in (9, 10) or y in (9, 10) else 0
        if not coverage:
            cx = 4 if x < 9 else 156
            cy = 4 if y < 9 else 156
            for sy in range(4):
                for sx in range(4):
                    dx, dy = x * 8 + sx * 2 + 1 - cx, y * 8 + sy * 2 + 1 - cy
                    coverage += dx * dx + dy * dy > 72 * 72
        image.putpixel((x, y), (255, 255, 255, (coverage * 255 + 8) // 16))
image.save(out / 'keymap-grid-junction.png')
print('Exported Zen 28/20 atlases, glyph metrics, exact 20px junction mask')
