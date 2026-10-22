"""Independently check every exported Unbounded glyph against its Unicode bitmap."""
from pathlib import Path
import json,re,sys
from PIL import Image
root=Path(sys.argv[1]);assets=Path(__file__).resolve().parents[1]/'assets/sm62'
for font in json.loads((assets/'clock-glyphs.json').read_text()):
 size=font['size'];s=(root/f'keyboards/ergohaven/macropad/sm62_font_unbounded_{size}.c').read_text()
 img=Image.open(assets/f'clock-unbounded-{size}.png')
 for cp,slot,w,h,dx,dy,adv in font['glyphs']:
  block=re.search(r'/\* U\+%04X .*?\*/(.*?)(?=/\*|\};)' % cp,s,re.S)[1]
  raw=bytes(int(v,16) for v in re.findall(r'0x[0-9a-fA-F]+',block))
  assert len(raw)*2>=w*h
  for p in range(w*h):
   a=((raw[p//2]>>4) if p%2==0 else raw[p//2]&15)*17
   assert img.getpixel((slot%8*64+p%w,slot//8*64+p//w))[3]==a,(size,cp,p)
 print('PASS original firmware glyph pixels:',size,len(font['glyphs']))
