# Spike report — issue #6: render → OCR → invisible text overlay

Status: **pipeline proven end-to-end on this dev machine.** The spike harness
(`examples/spike_ocr.rs`) renders each page with PDFium, OCRs it with Tesseract,
writes an invisible (render mode 3) text layer positioned to match the image,
and merges it onto the original page by appending the overlay ops to the
original content stream (`examples/merge_overlay.py`, a dev-time stand-in for
the production in-process Rust merge).

> Spike scope: the harness and fixtures prove the production pipeline design.
> The code is deliberately throwaway-ish (single-file example, CLI tools via
> `Command`), but every number and evidence artifact below is real output from
> this machine.

## 1. Exact tooling used (dev machine, x86_64 Linux)

| Component | Version | Path / source |
| --- | --- | --- |
| Rust toolchain | 1.96.0 | `rust-toolchain.toml` (repo) |
| pdfium-render | 0.9.4 | dev-dependency (only Cargo.toml change; runtime binding, no compile-time linking) |
| PDFium | 157.0.8100 (MAJOR=157 BUILD=8100, x64, no XFA/V8) | github.com/bblanchon/pdfium-binaries, untracked at `/tmp/opencode/pdfium/lib/libpdfium.so` |
| Tesseract | 5.5.0 (leptonica 1.86.0) | apt `tesseract-ocr`, CLI invocation |
| traineddata | eng + hun (osd) | apt `tesseract-ocr-eng`, `tesseract-ocr-hun` |
| glyphless overlay font | 572-byte `pdf.ttf` | `/usr/share/tesseract-ocr/5/tessdata/pdf.ttf` (the font Tesseract's own PDF renderer embeds) |
| poppler-utils (pdftotext 26.01.0, pdftoppm) | 26.01.0 | apt, verification only |
| mupdf-tools (mutool) | apt version | verification only (third independent extraction engine) |
| qpdf | 12.3.2 | apt, dissection of Tesseract's native PDFs; overlay merge initially attempted via `qpdf --overlay` (abandoned, see §6) |
| Python (verification) | 3.x + pillow, pikepdf, pypdf, fonttools | venv in `scratch/` (gitignored) |

Nothing here runs in the shipped app; the product requirement is that the
**shipped app never uses the network at runtime**, which the design honors:
PDFium is embedded, Tesseract is embedded or bundled with traineddata, and the
overlay writer is in-process Rust.

## 2. Test fixtures

`tests/fixtures/make_fixtures.py` regenerates four image-only PDFs (each a
single US-Letter page drawn as 144-DPI grayscale JPEG; all < 100 KB; verified
zero extractable characters before processing):

| Fixture | What it exercises |
| --- | --- |
| `eng_scan.pdf` (65 KB) | English body text |
| `hun_scan.pdf` (68 KB) | Hungarian incl. U+0151 **ő**, U+0171 **ű** and capitals **Ő/Ű** ("Őssel az őz…", "űrhajósok űrruhát…", "Árvíztűrő tükörfúrógép") |
| `rotated_scan.pdf` (57 KB) | MediaBox `[0 0 792 612]` stored sideways, `/Rotate 90`, so the bitmap must come out upright |
| `cropbox_scan.pdf` (54 KB) | CropBox `[36 36 576 756]` inset from MediaBox `[0 0 612 792]` |

## 3. Pipeline as implemented

```
input.pdf
  └─ PDFium render @300 DPI (applies /Rotate; bitmap = displayed page)
      └─ page_NN.jpg ──► tesseract -l eng+hun --dpi 300 tsv pdf txt
                          │ level-5 TSV word boxes (display px, y-down)
                          ▼
          display px → user-space pt:
            R0:   a = dx        b = H − dy
            R90:  a = dy        b = dx
            R180: a = W − dx    b = dy
            R270: a = H − dy    b = W − dx
            user = cropbox.left + a, cropbox.bottom + b
                          ▼
          overlay.pdf (hand-rolled writer: Type0/Identity-H glyphless font,
          per-word `BT 3 Tr /ChilliOcrF1 h Tf Tz Tm <UTF-16BE> Tj ET`, Tz chosen
          so the word advance equals its OCR box width)
      input.pdf ── append overlay ops to original content stream
                  (examples/merge_overlay.py, pikepdf;
                   production: same operation in-process in Rust)
                 ──► output.pdf
```

The merge appends each overlay page's content (wrapped in `q … Q`) to the
corresponding original page's content stream and copies the overlay font in
under `/ChilliOcrF1` — pure user-space composition with no coordinate
transforms. An earlier attempt used `qpdf --overlay`; that stamps in *display*
space and breaks rotated pages (§6).

### Overlay font technique (reverse-engineered from Tesseract's own output)

Tesseract's `TessPDFRenderer` output was dissected with `qpdf --qdf` and
pikepdf; the spike reproduces its object layout exactly:

- Type0 font, `/Encoding /Identity-H`, codes are the UTF-16BE code units of the
  text (so an **identity ToUnicode CMap** suffices: `<0000> <FFFF> <0000>`).
- Descendant `CIDFontType2` with `/DW 500` and a **CIDToGIDMap stream mapping
  every one of 65536 CIDs to GID 1** — the single invisible glyph `.null`
  (advance 1024/2048 em = 500/1000) in the 572-byte glyphless `pdf.ttf`.
  The font's cmap is empty; without this map every code would resolve to
  GID 0 (advance 0) and selection/advance math would collapse.
- `FontFile2` embeds `pdf.ttf` verbatim (572 bytes — no subsetting needed).
- Content: `3 Tr` (invisible), per-word `Tf` (font size), `Tz` (horizontal
  scale), `Tm` (position), `<hex> Tj` with a trailing space inside each word
  so plain-text extractors emit clean word breaks.

### Positioning math

- Baseline = bottom of the OCR word box (matches Tesseract: its `Tm` y equals
  `pageheight − (top+height)·72/300` for unrotated pages, verified on
  `eng_scan.pdf`).
- Font size = OCR box height in points ⇒ glyph ascent (1000/1000 em) exactly
  covers the box; selection highlight ≈ the OCR box.
- `Tz = 100 · box_width_pt / (0.5 · font_size · n_codes)` stretches the fixed
  0.5-em glyph advances to the word width, clamped to [10, 1000].
- Rotation: OCR boxes come back in *display* space (renderer already applied
  `/Rotate`), so each box is mapped back to user space with the inverse
  rotation (table above) and the CropBox origin offset. **Additionally the
  text matrix itself must be rotated**: the word's advance direction has to
  follow the displayed reading direction, which for `/Rotate 90` is user-space
  `+y`, not `+x`. The spike emits `Tm` rotation parts
  `1 0 0 1` / `0 1 -1 0` / `-1 0 0 -1` / `0 -1 1 0` for
  `/Rotate 0/90/180/270`. With an axis-aligned matrix, words on rotated pages
  come out transposed (advance 90° off), even when the anchor point is
  correct. This is the core of issue #13.

## 4. Verification evidence

All runs performed on this dev machine; artifacts kept under
`/tmp/opencode/spike6/out/<fixture>/` (run command below uses that layout).

```
CHILLI_MERGE_PY=scratch/venv/bin/python \
  target/debug/examples/spike_ocr <fixture>.pdf /tmp/opencode/spike6/out/<name>
scratch/venv/bin/python scratch/verify.py <fixture>.pdf /tmp/opencode/spike6/out/<name> \
  --expect <words…>
```

`spike_ocr` writes per page: rendered `page_01.jpg` (300 DPI),
`tess_01.{tsv,pdf,txt}` (Tesseract's own outputs; the native PDF is kept as the
strategy-B artifact), `overlay.pdf` (invisible-text layer),
`output.pdf` (merged searchable result), and prints the OCR text to stdout.

### 4.1 English fixture (`eng_scan.pdf`)

- 2550×3300 px render, 46 words, mean confidence 96.0.
- `pdftotext` extraction from `output.pdf` (first/last lines shown):

  ```
  Chilli PDF converts scanned documents into searchable PDFs.
  ...
  Offline OCR keeps every document on the local machine.
  ```

- pypdf `extract_text` finds the same text; expected words
  (`Chilli searchable Tesseract`) found by **both** extractors. ✔
- Word-box alignment (§4.5 method): 46 words matched, mean position error
  **1.52 pt** (worst 1.94 pt; threshold 3 pt). ✔
- Appearance: 150 DPI rasterization of page 1 pixel-identical to the input
  (`ImageChops.difference` → `None`). ✔
- Structure: image raw-stream MD5 identical, MediaBox/CropBox/`/Rotate`
  unchanged. ✔

### 4.2 Hungarian fixture (`hun_scan.pdf`) — ő/ű preservation

- 34 words, mean confidence 95.5. Extracted text (stdout, identical via
  pdftotext and pypdf):

  ```
  A gyors barna róka átugrik a lusta kutya fölött.
  Tűzön át visz az út, de csak óvatosan.
  Őssel az őz gyakran látható a szélén az erdőnek.
  Az űrhajósok űrruhát viseltek az űrállomáson.
  Árvíztűrő tükörfúrógép.
  ```

  **ő (U+0151) and ű (U+0171), capitals included, survive the full chain
  (render → OCR → UTF-16BE overlay → ToUnicode → extractor) unmangled.**
  Expected words `őz űrhajósok Árvíztűrő tükörfúrógép` found by both
  extractors; mean box error **1.90 pt**. ✔

### 4.3 Rotated fixture (`rotated_scan.pdf`, `/Rotate 90`)

- `/Rotate 90`, MediaBox `[0 0 792 612]`. PDFium renders the bitmap upright
  (2550×3300 px display-portrait); 22 words, mean confidence 96.0.
- Expected words `sideways rotation upright` found by both extractors. ✔
- Alignment checked against the `/Rotate`-stripped copy (§4.5): mean box
  error **1.93 pt** — i.e. the invisible words land exactly on the visually
  upright words *after* accounting for the page rotation, with the rotated
  `Tm` (`0 1 -1 0`) and inverse-rotation anchor mapping from §3. A sample
  content op from the overlay proves the shape of the emitted text:

  ```
  BT 3 Tr /ChilliOcrF1 13.68 Tf 105.26 Tz 0 1 -1 0 89.04 72.00 Tm
  <00540068006900730020> Tj ET        % "This "
  ```

- Caveat: on rotated pages pdftotext emits words in scrambled *order*
  (content intact, positions right); pypdf's and MuPDF's order is fine.
  Search and selection work correctly because per-word geometry is what
  matters.
- Appearance/structure checks pass as in 4.1. ✔

### 4.4 CropBox fixture (`cropbox_scan.pdf`)

- CropBox `[36 36 576 756]`, MediaBox `[0 0 612 792]`; the render covers the
  **CropBox** (2250×3000 px), confirmed empirically from bitmap dimensions.
- Expected words `crop inset align` found by both extractors; mean box
  error **1.84 pt** with word coordinates offset by the CropBox origin
  (verified: pdftotext reports crop-relative words at media-space
  coordinates exactly 36 pt larger). ✔

### 4.5 Verification method (`scratch/verify.py`, exit 0 = all pass)

1. **Text**: `pdftotext` and pypdf `extract_text` both contain each
   `--expect` word (substring check, case-sensitive).
2. **Geometry**: parse `pdftotext -bbox` XML from `output.pdf` and compare
   every word's box against the OCR-TSV box mapped display→user-space via
   the §3 table; a word matches its nearest same-text box; PASS if mean
   error < 3 pt. (Because poppler mangles `-bbox` coordinates on rotated
   pages — §6 — the comparison runs against a `/Rotate`-stripped copy
   produced with pikepdf. Residual ~1.5–2 pt error is a *convention*
   difference, not misplacement: poppler's glyph box is
   `[Tm.e, e + advance − trailing space] × [baseline − Ascent·fs/1000,
   baseline + |Descent|·fs/1000]` vs. Tesseract's ink box; a probe showed
   placement itself exact to the glyph box.)
3. **Appearance**: `pdftoppm -r 150` page 1 of input vs. output,
   `PIL.ImageChops.difference` → `getbbox() is None` (pixel-identical).
4. **Structure**: MediaBox/CropBox/`/Rotate` strings unchanged; JPEG image
   raw-stream MD5 multisets identical (recursing into Form XObjects).

5. **Third engine (viewers)**: `mutool draw -F txt` (MuPDF — the engine
   behind SumatraPDF etc.) extracts all four outputs with expected words
   present and correct line order, including the rotated page and ő/ű.
   Together with poppler (Evince/Okular) and pypdf, three independent
   implementations confirm the text layer is selectable/copyable.

All four fixtures: **ALL CHECKS PASSED** (`out/<name>/verify.txt`).

## 5. Strategy comparison (issue #6 acceptance criterion)

| | A: hOCR parse + own writer | B: Tesseract TessPDFRenderer (CLI `pdf` output) | **C: TSV word boxes + own writer (chosen)** |
| --- | --- | --- | --- |
| Extra parse layer | full XML/HTML parse of hOCR, bbox→line→word tree | none (library does it) | trivial tab-split of level-5 rows |
| Image passthrough | n/a (text-only overlay; original untouched) | **re-encodes the page image into a new PDF** — doubles size, recompression risk, must be merged anyway | original page untouched; overlay text-only |
| Positioning control | hOCR bboxes are the same TSV boxes dressed in XML; extra float-parsing and class-attr digging | fixed; fontsize from Tesseract's internal font estimate, Tm/Td relative | exact per-word `Tm` in user space; Tz derived from box width; rotation/CropBox handled explicitly |
| Encoding | same UTF-16BE + identity ToUnicode either way | handled by library | handled by spike writer |
| Merge step | still needed | still needed (its PDF is a *replacement*, not the original) | append overlay ops to original content stream (pikepdf helper in the spike; in-process Rust in production) |

**Decision: strategy C** — Tesseract TSV (or the equivalent API word boxes) +
our own minimal overlay writer + merge onto the original page. It is what
Tesseract itself does internally minus the hOCR detour, gives full control of
rotation/CropBox math (the actual hard part, issue #13), and never touches the
original page image. Strategy A adds an XML dependency for the same numbers.
Strategy B is a good **cross-check artifact** (kept as `tess_NN.pdf`) but as a
product approach it re-encodes the scan and still needs merging.

**Tr render mode: 3 (invisible)**, not 10 (invisible + clipping path). Mode 3
is what Tesseract and OCRmyPDF ship; mode 10 additionally pushes glyph
outlines to the clip path, which some viewers use for selection and is
meaningless for a glyphless font.

## 6. Pitfalls discovered

1. **qpdf `--overlay` stamps in display space, not user space.** On the
   `/Rotate 90` fixture, qpdf wrapped the original content as
   `q 0 1 -1 0 792 0 cm /Fx0 Do Q` and the overlay as
   `q 0 0.77273 -0.77273 0 632.45455 0 cm /Fx1 Do Q` (rotation plus a
   612/792 squash) — text landed visibly misplaced. Unrotated pages were
   fine (identity transform, 0.00 pt error), which hides the bug until you
   test rotation. Merge by *appending* overlay ops to the original content
   stream instead (what the spike and the production design now do).
2. **Tesseract's `TessPDFRenderer` (CLI `pdf` output) writes the page in
   bitmap/display orientation.** For the `/Rotate 90` fixture its output is
   a fresh 612×792 portrait page with axis-aligned text, ignoring the
   original MediaBox `[0 0 792 612]` and `/Rotate`. It replaces the original
   image (re-encoded) and cannot be overlaid onto the original page. Two
   independent strikes against strategy B.
3. **Rotated pages need a rotated text matrix** (§3): OCR boxes arrive in
   display space; an axis-aligned `1 0 0 1 Tm` transposes words on `/Rotate`
   pages even with a correct anchor.
4. **`FPDFPage_GetCropBox` (pdfium-render `page.boundaries().crop()`) errors
   when the page has no CropBox** rather than falling back to the MediaBox.
   Production must fall back crop→media explicitly (PDF 7.7.3.3), as the
   spike now does.
5. **CIDToGIDMap is mandatory with the glyphless font.** `pdf.ttf`'s cmap
   tables are empty; without the 131 072-byte all-GID-1 map every code
   resolves to GID 0 (advance 0) and word advances collapse. Tesseract's
   trick: one `bfrange` identity ToUnicode CMap (`<0000> <FFFF> <0000>`)
   plus UTF-16BE codes, so any text — including ő/ű and surrogate pairs —
   round-trips.
6. **Trailing space per word.** Each word string gets one trailing space so
   plain-text extractors emit word breaks; poppler's glyph box excludes it
   (part of the ~1.5–2 pt convention residual in §4).
7. **Poppler `pdftotext -bbox` quirks**: coordinates are raw MediaBox user
   space with y *down from the media top*; on rotated pages it applies a
   confusing scale+offset (display/user dims, e.g. 612/792) instead of raw
   boxes. Verification (and any test tooling for #13) should compare against
   a `/Rotate`-stripped copy. `pdftotext` also scrambles word *order* on
   rotated pages (content/positions still correct).
8. **pikepdf `page.Rotation = …` (capital R) silently does nothing**;
   lowercase `page.rotation` persists. (Fixture-generation bug; cost a
   misleading round of "unrotated" test results.)
9. **Fixture text must be width-fitted**: initial 48 px font overflows the
   usable width and long lines get clipped by the rasterizer — OCR then
   invents shorter words. `make_fixtures.py` now auto-fits font size.
10. **Ubuntu AppArmor confines qpdf** (`/etc/apparmor.d/qpdf`: paths under
    `/tmp/**.pdf` or non-hidden `$HOME` dirs only). Working under a
    dot-directory (`~/.herdr/...`) yields `Permission denied` even for
    readable files. Not relevant to production, but a dev-machine trap.
11. **JPEG renders carry no DPI metadata** — always pass `--dpi 300` to
    Tesseract explicitly, or box font sizes come out wrong.

## 7. Performance notes

Timed single runs (this shared dev VM is under heavy load from parallel agents;
PDFium render times are inflated — OCR/overlay/merge are the meaningful
numbers). Page content: ~4–5 lines of text on one US-Letter page.

| Fixture | render (PDFium) | OCR (tesseract, tsv+pdf+txt in one pass) | overlay write | merge | total |
| --- | --- | --- | --- | --- | --- |
| eng (46 words) | 11.6 s | 7.0 s | < 0.1 s | 0.94 s | ~20 s |
| hun (34 words) | 12.0 s | 8.7 s | < 0.1 s | 0.72 s | 21.5 s |
| rotated (22 words) | 12.9 s | 5.2 s | < 0.1 s | 0.74 s | ~19 s |
| cropbox (29 words) | 9.7 s | 4.6 s | < 0.1 s | 0.86 s | ~15 s |

- **The expensive step is OCR, not the overlay.** The hand-rolled overlay
  PDF (137 KB eng, includes the 131 KB CIDToGIDMap) writes in well under
  100 ms; merge via pikepdf under 1 s (includes re-parsing both PDFs — an
  in-process merge over already-loaded documents will be far cheaper).
- Tesseract ran with three outputs (`tsv pdf txt`) in one pass for evidence
  gathering; production would request only word boxes, shaving time.
- The 131 KB raw CIDToGIDMap dominates overlay size (one font object shared
  by all pages, so it is per-*document*). Tesseract ships the same map
  Flate-compressed (the repeated two-byte pattern compresses to well under
  1 KB); production should compress it the same way.
- Rendering cost scales with DPI and page area; at 300 DPI a letter page is
  ~2550×3300 px (~25 Mpx). Production should reuse one PDFium + one
  Tesseract instance across all pages and threads (PDFium is not
  thread-safe per document).

## 8. Recommendations for follow-up issues

- **#7 (PDFium)**: bind `libpdfium.so` at runtime via pdfium-render as in the
  spike; pin a pdfium-binaries build; keep the binary out of git (untracked
  scratch + bundling handled by the asset workstream).
- **#8 (Tesseract)**: prefer the C API (`libtesseract`) over CLI in production
  (the CLI is fine for the spike); `-l eng+hun` works well on both fixtures;
  use `--dpi 300` explicitly since JPEGs carry no DPI metadata.
- **#13 (overlay positioning)**: implement the display→user-space inverse
  rotation table, CropBox/MediaBox covered-box detection, rotated `Tm`
  matrices, and `Tz` width-matching from §3; merge by appending overlay ops
  to the original content stream (never `qpdf --overlay`, which stamps in
  display space — §6). That is the whole positioning story and it is fully
  verified in §4.
