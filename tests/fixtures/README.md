# OCR test fixtures

## `pdf.ttf` — glyphless overlay font

The 572-byte glyphless TrueType font Tesseract ships (in the `tesseract-ocr`
package) for its searchable-PDF output, used by `tests/overlay.rs` as the
default font-bytes resolver for the invisible-text writer (issue #13).

Provenance: copied verbatim from `/usr/share/tesseract-ocr/5/tessdata/pdf.ttf`
(tesseract-ocr 5.x on Debian/Ubuntu). It contains a single `.null` glyph with a
non-zero advance width and no outlines at all, which is exactly what an
invisible (`Tr 3`) text layer needs: every text code maps to the one glyph, so
`Tz` scaling controls the advance and nothing is ever drawn.

License: Apache License 2.0 — same as Tesseract itself (see
`THIRD_PARTY_NOTICES.md`). In production the font is bundled as an application
asset (src/assets.rs, issue #9), never read from this fixture.

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

## The `gen_*.pdf` synthetic scan fixtures

Five small scanned-style PDFs generated **in pure Rust** by
`tests/fixtures_gen.rs` (issue #18). They exist so the pipeline integration
tests (`tests/integration.rs`) can exercise the real conversion path —
validation, rendering seams, invisible overlay merge, transactional publish —
without Python, Pillow, Tesseract, or any system OCR library.

| Fixture | Shape |
| --- | --- |
| `gen_eng.pdf` | 1 letter image page (scan-like pattern) |
| `gen_hun.pdf` | 1 letter image page (ragged text-like pattern) |
| `gen_hybrid.pdf` | page 1 = real Helvetica text layer, page 2 = image page |
| `gen_rotated.pdf` | 1 landscape page (`[0 0 792 612]`) tagged `/Rotate 90` |
| `gen_multi.pdf` | 5 letter image pages sharing one image XObject |

Notes on the design:

* Every page is "scanned-style": a tiny deterministic 64×64 8-bit grayscale
  image XObject, uncompressed, with no timestamps anywhere, so regeneration
  is byte-for-byte reproducible.
* The Hungarian `ő`/`ű` coverage lives in the *scripted OCR results* the
  integration tests feed through the pipeline (the fixtures are images, not
  text) — the round trip is exercised end to end and asserted on the decoded
  overlay content stream.
* The tests never embed real document text; every string asserted on is a
  synthetic fixture-owned word defined alongside the tests.

Regenerate after editing the generators in `tests/fixtures_gen.rs`:

```sh
CHILLI_REGEN_FIXTURES=1 cargo test --test fixtures_gen
```

With the variable unset (the default, and how CI runs), the same test only
verifies that the committed files are byte-identical to what the generator
produces — so a hand-edited or stale fixture fails loudly instead of
mysteriously breaking the integration tests.

Each fixture is well under 100 KB and the whole set stays under 2 MB, keeping
checkouts and CI fast.

## The `*_scan.pdf` fixtures and `make_fixtures.py`

`eng_scan.pdf`, `hun_scan.pdf`, `rotated_scan.pdf`, `cropbox_scan.pdf`, and
`encrypted_scan.pdf` were produced by `make_fixtures.py` (Python + Pillow +
pikepdf) during the overlay work (issue #13) and carry PIL-derived
ground-truth word tables hardcoded in `tests/overlay.rs`. **Do not
regenerate or hand-edit them** — that would invalidate those tables.
`make_fixtures.py` remains a dev-time spike tool; nothing in the test suite
depends on Python. `encrypted_scan.pdf` is a real password-protected PDF,
which `tests/integration.rs` reuses as the pipeline-level encrypted-input
negative case.

