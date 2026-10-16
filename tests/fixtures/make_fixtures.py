#!/usr/bin/env python3
"""Regenerate the synthetic "scanned" PDF fixtures for the issue #6 spike.

Fixtures are image-only PDFs (no text layer) that simulate flatbed scans:
  * eng_scan.pdf      - English text
  * hun_scan.pdf      - Hungarian text including U+0151 (ő) and U+0171 (ű),
                        plus capital Ő and Ű
  * rotated_scan.pdf  - page stored sideways with /Rotate 90 so it displays
                        upright, like a landscape scan of a portrait page
  * cropbox_scan.pdf  - page with a CropBox inset from the MediaBox

Each fixture is a single page, US Letter, rendered as a grayscale JPEG at
144 "scan" DPI, well under 100 KB.

Deps: pip install pillow pikepdf   (dev-time only; the shipped app never runs
this script and never touches the network).
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont
import pikepdf

OUT = Path(__file__).parent
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"

# 2 px per point == 144 DPI scan; US Letter is 612x792 pt.
SCALE = 2
PAGE_W, PAGE_H = 612 * SCALE, 792 * SCALE  # 1224 x 1584 px
MARGIN = 72 * SCALE  # 1 inch
USABLE_W = PAGE_W - 2 * MARGIN
LINE_SPACING = 1.8

TEXTS = {
    "eng_scan.pdf": [
        "Chilli PDF converts scanned documents into searchable PDFs.",
        "The quick brown fox jumps over the lazy dog.",
        "Searchable text can be selected and copied from the output.",
        "Offline OCR keeps every document on the local machine.",
        "Tesseract reads the page; PDFium renders it; the overlay aligns.",
    ],
    "hun_scan.pdf": [
        "A gyors barna róka átugrik a lusta kutya fölött.",
        "Tűzön át visz az út, de csak óvatosan.",
        "Őssel az őz gyakran látható a szélén az erdőnek.",
        "Az űrhajósok űrruhát viseltek az űrállomáson.",
        "Árvíztűrő tükörfúrógép.",
    ],
    "rotated_scan.pdf": [
        "This page is stored sideways and marked rotated.",
        "The quick brown fox appears upright after rotation.",
        "Rendering applies the rotation attribute correctly.",
    ],
    "cropbox_scan.pdf": [
        "This page has a crop box inset from the media box.",
        "The crop box trims the outer half inch of paper.",
        "Text must align inside the cropped visible area.",
    ],
}


def fit_font(lines: list[str]) -> ImageFont.FreeTypeFont:
    """Largest font (<= 24 pt) whose widest line fits the usable width."""
    size = 24 * SCALE
    while size > 8:
        font = ImageFont.truetype(FONT, size)
        if max(font.getlength(line) for line in lines) <= USABLE_W:
            return font
        size -= 2
    raise SystemExit("fixture lines cannot fit the page")


def make_page(lines: list[str]) -> Image.Image:
    """Draw lines of text on a white page, return the PIL image."""
    img = Image.new("L", (PAGE_W, PAGE_H), 255)
    draw = ImageDraw.Draw(img)
    font = fit_font(lines)
    y = MARGIN
    for line in lines:
        draw.text((MARGIN, y), line, font=font, fill=0)
        y += int(font.size * LINE_SPACING)
    return img


def save_pdf(img: Image.Image, path: Path) -> None:
    img.save(path, format="PDF", resolution=72.0 * SCALE)


def main() -> None:
    for name, lines in TEXTS.items():
        path = OUT / name
        if name == "rotated_scan.pdf":
            # Simulate a scan fed sideways: rotate the image content 90°
            # counter-clockwise (PIL rotate(90) is CCW), then set /Rotate 90
            # (90° clockwise display rotation) so the page reads upright again.
            img = make_page(lines).rotate(90, expand=True)
            save_pdf(img, path)
            with pikepdf.open(path, allow_overwriting_input=True) as pdf:
                pdf.pages[0].rotation = 90
                pdf.save(path)
        elif name == "cropbox_scan.pdf":
            img = make_page(lines)
            save_pdf(img, path)
            with pikepdf.open(path, allow_overwriting_input=True) as pdf:
                # 0.5 inch inset from the MediaBox [0 0 612 792].
                pdf.pages[0].CropBox = [36, 36, 576, 756]
                pdf.save(path)
        else:
            save_pdf(make_page(lines), path)
        print(f"{path}  {path.stat().st_size} bytes")


if __name__ == "__main__":
    main()
