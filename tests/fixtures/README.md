# OCR test fixtures

## `hungarian_ou.png`

A 300 DPI, 8-bit grayscale render of two Hungarian lines containing both
double-acute letters, used by `tests/ocr.rs` to prove `ő` and `ű` survive the
full OCR pipeline:

```
árvíztűrő tükörfúrógép
gőzgép és űrhajó
```

Regenerate with Python + Pillow (DejaVu Sans supports the Hungarian
diacritics):

```sh
python3 - <<'PY'
from PIL import Image, ImageDraw, ImageFont

font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 60)
lines = ["árvíztűrő tükörfúrógép", "gőzgép és űrhajó"]
margin, line_gap = 28, 44

meas = Image.new("L", (1, 1))
md = ImageDraw.Draw(meas)
boxes = [md.textbbox((0, 0), ln, font=font) for ln in lines]
line_h = max(b[3] - b[1] for b in boxes)
W = max(b[2] - b[0] for b in boxes) + 2 * margin
H = len(lines) * line_h + (len(lines) - 1) * line_gap + 2 * margin

img = Image.new("L", (W, H), color=255)
draw = ImageDraw.Draw(img)
y = margin
for ln, b in zip(lines, boxes):
    draw.text((margin - b[0], y - b[1]), ln, font=font, fill=0)
    y += line_h + line_gap

img.save("tests/fixtures/hungarian_ou.png", dpi=(300, 300), optimize=True)
PY
```
