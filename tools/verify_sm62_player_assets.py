#!/usr/bin/env python3
"""Independently check embedded player assets against read-only firmware arrays.
Usage: python3 tools/verify_sm62_player_assets.py PATH_TO_FIRMWARE
"""
import json
from pathlib import Path
import re
import sys
from PIL import Image
root = Path(sys.argv[1])
out = Path(__file__).resolve().parents[1] / 'assets/sm62'
fonts = json.loads((out/'player-glyphs.json').read_text())
count = pixels = 0
for font in fonts:
    text = (root/f"keyboards/ergohaven/macropad/sm62_font_zen_{font['name']}.c").read_text()
    raw = re.search(r'glyph_bitmap\[\] = \{(.*?)\n\};', text, re.S)[1]
    raw = re.sub(r'/\*.*?\*/', '', raw, flags=re.S)
    bitmap = bytes(int(v,16) for v in re.findall(r'0x[0-9a-fA-F]+',raw))
    raw = re.search(r'glyph_dsc\[\] = \{(.*?)\n\};',text,re.S)[1]
    descriptors = [dict((k,int(v)) for k,v in re.findall(r'\.(\w+)\s*=\s*(-?\d+)',entry)) for entry in re.findall(r'\{([^{}]+)\}',raw)]
    line = int(re.search(r'\.line_height = (\d+)',text)[1])
    base = int(re.search(r'\.base_line = (\d+)',text)[1])
    assert (font['line_height'],font['base_line']) == (line,base)
    assert len(font['glyphs']) == 95
    image = Image.open(out/f"player-zen-{font['name']}.png").convert('RGBA')
    assert image.size == (512,240)
    for cp,slot,w,h,dx,dy,adv in font['glyphs']:
        # All four real C cmaps begin with contiguous ASCII 32..127 at glyph 1.
        d = descriptors[cp-31]
        assert [w,h,dx,dy,adv] == [d['box_w'],d['box_h'],d['ofs_x'],line-base-d['box_h']-d['ofs_y'],(d['adv_w']+8)//16]
        for i in range(w*h):
            b=bitmap[d['bitmap_index']+i//2]
            alpha=((b>>4) if i%2==0 else (b&15))*17
            assert image.getpixel((slot%16*32+i%w,slot//16*40+i//w)) == (255,255,255,alpha)
            pixels += 1
        count += 1
print(f'PASS: {count} glyph metrics and {pixels} bitmap pixels match actual firmware C arrays')
