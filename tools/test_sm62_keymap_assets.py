#!/usr/bin/env python3
"""Check exported atlas against independently identified firmware glyph blocks."""
import json
import re
import sys
import unittest
from pathlib import Path
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
FIRMWARE = Path(sys.argv.pop(1))

class FirmwareAtlasTests(unittest.TestCase):
    def test_cyrillic_glyphs_match_firmware_unicode_blocks(self):
        fonts = json.loads((ROOT / 'assets/sm62/keymap-glyphs.json').read_text())
        for font in fonts:
            size = font['size']
            source = (FIRMWARE / f'keyboards/ergohaven/macropad/sm62_font_zen_medium_{size}.c').read_text()
            image = Image.open(ROOT / f'assets/sm62/keymap-zen-{size}.png')
            for c in 'ЙЦУФЫВЯЧСНГШРОЛТЬБ':
                with self.subTest(size=size, glyph=c):
                    cp, slot, w, h, *_ = next(g for g in font['glyphs'] if g[0] == ord(c))
                    block = re.search(r'/\* U\+%04X .*?\*/(.*?)(?=/\*|\};)' % cp, source, re.S)[1]
                    raw = bytes(int(v, 16) for v in re.findall(r'0x[0-9a-fA-F]+', block))
                    self.assertGreaterEqual(len(raw) * 2, w * h, 'exported glyph geometry does not match Unicode bitmap')
                    expected = [((raw[p // 2] >> 4) if p % 2 == 0 else (raw[p // 2] & 15)) * 17 for p in range(w * h)]
                    actual = [image.getpixel((slot % 16 * 32 + p % w, slot // 16 * 40 + p // w))[3] for p in range(w * h)]
                    self.assertEqual(actual, expected)

if __name__ == '__main__':
    unittest.main()
