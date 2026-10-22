"""Export original Unbounded48/16 uncompressed LVGL glyphs for SM62 clock."""
from pathlib import Path
import re,json,sys
from PIL import Image
root=Path(sys.argv[1]);out=Path(__file__).resolve().parents[1]/'assets/sm62';fonts=[]
for size in (48,16):
 source=root/f'keyboards/ergohaven/macropad/sm62_font_unbounded_{size}.c';s=source.read_text()
 raw=re.search(r'glyph_bitmap\[\] = \{(.*?)\n\};',s,re.S)[1]
 cps=[int(v,16) for v in re.findall(r'/\* U\+([0-9A-Fa-f]+)',raw)]
 raw=re.sub(r'/\*.*?\*/','',raw,flags=re.S)
 data=bytes(int(v,16) for v in re.findall(r'0x[0-9a-fA-F]+',raw))
 d=re.search(r'glyph_dsc\[\] = \{(.*?)\n\};',s,re.S)[1]
 ds=[dict((k,int(v)) for k,v in re.findall(r'\.(\w+)\s*=\s*(-?\d+)',e)) for e in re.findall(r'\{([^{}]+)\}',d)]
 assert len(ds)==len(cps)+1
 assert '.kern_dsc = NULL' in s and '.bitmap_format = 0' in s
 lh=int(re.search(r'\.line_height = (\d+)',s)[1]);bl=int(re.search(r'\.base_line = (\d+)',s)[1])
 img=Image.new('RGBA',(512,128));metrics=[]
 for slot,cp in enumerate(cps):
  g=ds[slot+1];w,h=g['box_w'],g['box_h'];assert w<=64 and h<=64
  for p in range(w*h):
   byte=data[g['bitmap_index']+p//2];alpha=((byte>>4) if p%2==0 else byte&15)*17
   img.putpixel((slot%8*64+p%w,slot//8*64+p//w),(255,255,255,alpha))
  metrics.append([cp,slot,w,h,g['ofs_x'],lh-bl-h-g['ofs_y'],(g['adv_w']+8)//16])
 img.save(out/f'clock-unbounded-{size}.png');fonts.append({'size':size,'glyphs':metrics})
(out/'clock-glyphs.json').write_text(json.dumps(fonts,separators=(',',':'))+'\n')
print('Exported original Unbounded48/16 bitmaps, advances and baseline offsets')
