#!/usr/bin/env python3
"""Merge an invisible-text overlay onto the original pages of a PDF (dev-time
spike helper for chilli-pdf issue #6).

Usage: merge_overlay.py OVERLAY.pdf INPUT.pdf OUTPUT.pdf

For each page, the overlay page's content stream is APPENDED to the original
page's content (after a q/Q wrap, so graphics state cannot leak) and the
overlay's font resource is copied in under a collision-proof name. Everything
else in the original file — page dicts, boxes, /Rotate, image streams — is
left untouched.

This is a user-space merge: unlike `qpdf --overlay`, which stamps in display
orientation (and bakes a rotate+scale transform into the output for pages with
/Rotate), appending the overlay ops directly keeps the text aligned with the
page's own coordinate system for every rotation and cropbox. The production
pipeline (issue #13) should do the same append in-process.

Requires pikepdf (dev-time only; the shipped app performs this merge itself
and must stay dependency-free).
"""

import sys

import pikepdf

FONT_NAME = "/ChilliOcrF1"


def stream_list(contents):
    """Normalized list of content streams for a page's /Contents."""
    if isinstance(contents, pikepdf.Array):
        return list(contents)
    return [contents]


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__, file=sys.stderr)
        return 2
    overlay_path, input_path, output_path = sys.argv[1:4]

    overlay = pikepdf.open(overlay_path)
    base = pikepdf.open(input_path)

    if len(overlay.pages) != len(base.pages):
        print(
            f"page count mismatch: overlay {len(overlay.pages)} vs input {len(base.pages)}",
            file=sys.stderr,
        )
        return 1

    for opage, bpage in zip(overlay.pages, base.pages):
        # Append the overlay content ops inside a q/Q save/restore so the
        # original content's graphics state (CTM, Tr, fonts) cannot leak either way.
        ops = b"\nq\n" + b"".join(s.read_bytes() for s in stream_list(opage.Contents)) + b"\nQ\n"
        bpage.contents_add(ops, prepend=False)

        # Copy the font (Type0 + descendant + descriptor + font file all come
        # along via the object graph) under a name nothing else uses.
        ofonts = opage.Resources.Font
        bfonts = bpage.get("/Resources", pikepdf.Dictionary()).get(
            "/Font", pikepdf.Dictionary()
        )
        if FONT_NAME in bfonts:
            print(f"{FONT_NAME} already present in input page resources", file=sys.stderr)
            return 1
        if "/Resources" not in bpage:
            bpage.Resources = pikepdf.Dictionary()
        if "/Font" not in bpage.Resources:
            bpage.Resources.Font = pikepdf.Dictionary()
        bpage.Resources.Font[FONT_NAME] = base.copy_foreign(ofonts[FONT_NAME])

    base.save(output_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
