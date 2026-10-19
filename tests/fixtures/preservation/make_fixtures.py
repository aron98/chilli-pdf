#!/usr/bin/env python3
"""Regenerate the preservation fixtures for issue #14.

Four minimal, hand-written, classic-xref PDFs that exercise what the
overlay merge must NOT touch. Deterministic byte output (no timestamps,
no dependencies) so the committed binaries are exactly reproducible:

  * metadata.pdf           - Info dict: /Title with UTF-16BE Hungarian
                             o-double-acute (U+0151) and u-double-acute
                             (U+0171), /Author, /CreationDate, /ModDate,
                             and a custom /Company key; two pages whose
                             /Resources carry an existing /F1 font.
  * outline.pdf            - 3-level outline/bookmark tree pointing at
                             pages 1 and 2 (one dest array, one GoTo
                             action).
  * links.pdf              - internal GoTo link (direct /Dest array), a
                             GoTo action link (/A << /S /GoTo /D ... >>),
                             and an external URI link annotation.
  * rotated_metadata.pdf   - /Rotate 90 landscape page together with an
                             Info dict (rotation AND metadata).

Each fixture is a couple of KB, loads in lopdf, and passes
`qpdf --check`. Dev-time only; the shipped app never runs this script.
"""

from pathlib import Path

OUT = Path(__file__).parent


def utf16be_hex(text: str) -> str:
    """A PDF text string (UTF-16BE with byte-order mark) as a hex string."""
    return ("<" + (b"\xfe\xff" + text.encode("utf-16-be")).hex().upper() + ">")


def stream(data: bytes) -> bytes:
    return b"<< /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"


def page(parent: int, mediabox: str, contents: int, resources: str, extra: str = "") -> bytes:
    return (
        f"<< /Type /Page /Parent {parent} 0 R /MediaBox {mediabox} "
        f"/Resources {resources} /Contents {contents} 0 R{extra} >>"
    ).encode()


def build(objects: dict[int, bytes], root: int, info: int | None = None) -> bytes:
    """Serialize objects with a real classic cross-reference table."""
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets: dict[int, int] = {}
    for num in sorted(objects):
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode()
        out += objects[num]
        out += b"\nendobj\n"
    xref_at = len(out)
    top = max(objects)
    out += b"xref\n"
    out += f"0 {top + 1}\n".encode()
    out += b"0000000000 65535 f \n"
    for num in range(1, top + 1):
        out += f"{offsets[num]:010d} 00000 n \n".encode()
    trailer = f"/Size {top + 1} /Root {root} 0 R"
    if info is not None:
        trailer += f" /Info {info} 0 R"
    out += f"trailer\n<< {trailer} >>\nstartxref\n{xref_at}\n%%EOF\n".encode()
    return bytes(out)


LETTER = "[0 0 612 792]"
EMPTY_CONTENT = b"q\nQ\n"


def metadata_pdf() -> bytes:
    info = (
        "<< /Title "
        + utf16be_hex("Teszt dokumentum: ő és ű")
        + " /Author (Chilli Teszt) "
        + "/CreationDate (D:20260301120000Z) /ModDate (D:20260615120000Z) "
        + "/Company (Chilli Kft.) /Producer (make_fixtures.py) >>"
    ).encode()
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        3: page(2, LETTER, 5, "7 0 R"),
        4: page(2, LETTER, 6, "7 0 R"),
        5: stream(EMPTY_CONTENT),
        6: stream(EMPTY_CONTENT),
        # Indirect shared resources with a pre-existing font: the overlay
        # must keep /F1 and only add its own entry.
        7: b"<< /Font << /F1 8 0 R >> >>",
        8: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        9: info,
    }
    return build(objects, root=1, info=9)


def outline_pdf() -> bytes:
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        3: page(2, LETTER, 5, "<< >>"),
        4: page(2, LETTER, 6, "<< >>"),
        5: stream(EMPTY_CONTENT),
        6: stream(EMPTY_CONTENT),
        7: b"<< /Type /Outlines /First 8 0 R /Last 11 0 R /Count 4 >>",
        # Level 1 -> page 1 (dest array).
        8: (
            b"<< /Title (Egyes oldal) /Parent 7 0 R /Next 11 0 R "
            b"/Dest [3 0 R /XYZ 0 792 0] /First 9 0 R /Last 9 0 R /Count 2 >>"
        ),
        # Level 2 -> page 1 (dest array).
        9: (
            b"<< /Title (Kettes alresz) /Parent 8 0 R "
            b"/Dest [3 0 R /XYZ 0 700 0] /First 10 0 R /Last 10 0 R /Count 1 >>"
        ),
        # Level 3 -> page 1 (dest array).
        10: b"<< /Title (Harmas szint) /Parent 9 0 R /Dest [3 0 R /XYZ 0 600 0] >>",
        # Level 1 -> page 2 (GoTo action).
        11: b"<< /Title (Negyedes oldal) /Parent 7 0 R /Prev 8 0 R /A << /S /GoTo /D [4 0 R /Fit] >> >>",
    }
    return build(objects, root=1)


def links_pdf() -> bytes:
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        3: page(2, LETTER, 5, "<< >>", extra=" /Annots [7 0 R 8 0 R]"),
        4: page(2, LETTER, 6, "<< >>", extra=" /Annots [9 0 R]"),
        5: stream(EMPTY_CONTENT),
        6: stream(EMPTY_CONTENT),
        # Internal GoTo link via a direct /Dest array (page 2, XYZ).
        7: (
            b"<< /Type /Annot /Subtype /Link /Rect [72 680 220 720] "
            b"/Border [0 0 0] /Dest [4 0 R /XYZ 100 700 0] >>"
        ),
        # Internal GoTo link via a /A GoTo action (page 2, Fit).
        8: (
            b"<< /Type /Annot /Subtype /Link /Rect [300 680 500 720] "
            b"/Border [0 0 0] /A << /S /GoTo /D [4 0 R /Fit] >> >>"
        ),
        # External URI link.
        9: (
            b"<< /Type /Annot /Subtype /Link /Rect [72 380 450 420] "
            b"/Border [0 0 0] /A << /S /URI /URI (https://example.com/chilli) >> >>"
        ),
    }
    return build(objects, root=1)


def rotated_metadata_pdf() -> bytes:
    info = (
        "<< /Title "
        + utf16be_hex("Forgatott teszt ű")
        + " /Author (Chilli Teszt) "
        + "/CreationDate (D:20260501120000Z) /ModDate (D:20260501120001Z) >>"
    ).encode()
    objects = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: page(2, "[0 0 792 612]", 4, "<< >>", extra=" /Rotate 90"),
        4: stream(EMPTY_CONTENT),
        5: info,
    }
    return build(objects, root=1, info=5)


FIXTURES = {
    "metadata.pdf": metadata_pdf,
    "outline.pdf": outline_pdf,
    "links.pdf": links_pdf,
    "rotated_metadata.pdf": rotated_metadata_pdf,
}


def main() -> None:
    for name, make in FIXTURES.items():
        path = OUT / name
        path.write_bytes(make())
        print(f"{path}  {path.stat().st_size} bytes")


if __name__ == "__main__":
    main()
