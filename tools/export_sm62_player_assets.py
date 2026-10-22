#!/usr/bin/env python3
"""Read-only actual C/LVGL export of player ASCII bitmaps and kerned metrics.
Usage: python3 tools/export_sm62_player_assets.py PATH_TO_FIRMWARE
Requires cc and Pillow. Scratch files live below TMPDIR.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from PIL import Image

root = Path(sys.argv[1]).resolve()
output = Path(__file__).resolve().parents[1] / 'assets/sm62'
names = ('regular_20', 'medium_20', 'regular_24', 'bold_24')
source = r'''
#include <stdio.h>
#include "lvgl.h"
LV_FONT_DECLARE(sm62_font_zen_regular_20);
LV_FONT_DECLARE(sm62_font_zen_medium_20);
LV_FONT_DECLARE(sm62_font_zen_regular_24);
LV_FONT_DECLARE(sm62_font_zen_bold_24);
int main(void) {
 const lv_font_t *fonts[] = {&sm62_font_zen_regular_20,&sm62_font_zen_medium_20,&sm62_font_zen_regular_24,&sm62_font_zen_bold_24};
 for(int f=0;f<4;f++) {
  printf("F %d %d %d\n",f,fonts[f]->line_height,fonts[f]->base_line);
  for(int c=32;c<127;c++) {
   lv_font_glyph_dsc_t d;
   if(!lv_font_get_glyph_dsc(fonts[f],&d,c,0)) return 2;
   printf("G %d %d %d %d %d %d %d ",c,d.box_w,d.box_h,d.ofs_x,d.ofs_y,d.adv_w,d.bpp);
   const unsigned char *b=lv_font_get_glyph_bitmap(fonts[f],c);
   for(int i=0;i<(d.box_w*d.box_h+1)/2;i++) printf("%02x",b[i]);
   printf("\n");
   for(int n=32;n<127;n++) {
    int advance=lv_font_get_glyph_width(fonts[f],c,n);
    if(advance!=d.adv_w) printf("K %d %d %d\n",c,n,advance);
   }
  }
 }
}
'''
with tempfile.TemporaryDirectory(prefix='sm62-player-', dir=os.environ.get('TMPDIR')) as temp:
    temp = Path(temp)
    (temp / 'export.c').write_text(source)
    (temp / 'lv_conf.h').write_text('''#ifndef LV_CONF_H
#define LV_CONF_H
#define LV_COLOR_DEPTH 16
#define LV_USE_LOG 0
#define LV_USE_ASSERT_NULL 0
#define LV_USE_ASSERT_MALLOC 0
#define LV_USE_FONT_COMPRESSED 0
#endif
''')
    lv = root / 'lib/lvgl'
    subprocess.run(['cc', '-std=c99', '-O2', '-DLV_CONF_INCLUDE_SIMPLE', '-I'+str(temp), '-I'+str(lv),
        str(temp/'export.c'), str(lv/'src/font/lv_font.c'), str(lv/'src/font/lv_font_fmt_txt.c'),
        str(lv/'src/misc/lv_utils.c'), '-ffunction-sections', '-fdata-sections', '-Wl,--gc-sections',
        *(str(root/f'keyboards/ergohaven/macropad/sm62_font_zen_{n}.c') for n in names),
        '-o', str(temp/'export')], check=True)
    raw = subprocess.check_output([str(temp/'export')], text=True)
fonts = []
for line in raw.splitlines():
    p = line.split()
    if p[0] == 'F':
        font = {'name':names[int(p[1])], 'line_height':int(p[2]), 'base_line':int(p[3]), 'glyphs':[], 'kern':[]}
        fonts.append(font)
        atlas = Image.new('RGBA', (512, 240))
    elif p[0] == 'G':
        cp, w, h, dx, ofs_y, advance, bpp = map(int, p[1:8])
        assert bpp == 4
        slot = cp - 32
        bitmap = bytes.fromhex(p[8]) if len(p)>8 else b''
        for i in range(w*h):
            alpha = ((bitmap[i//2] >> 4) if i%2==0 else (bitmap[i//2] & 15))*17
            atlas.putpixel((slot%16*32+i%w, slot//16*40+i//w), (255,255,255,alpha))
        font['glyphs'].append([cp, slot, w, h, dx, font['line_height']-font['base_line']-h-ofs_y, advance])
        atlas.save(output / f"player-zen-{font['name']}.png")
    else:
        font['kern'].append(list(map(int,p[1:])))
(output/'player-glyphs.json').write_text(json.dumps(fonts,separators=(',',':'))+'\n')
print('Exported four actual C/LVGL font atlases, 380 ASCII glyphs, and pair-dependent advances')
