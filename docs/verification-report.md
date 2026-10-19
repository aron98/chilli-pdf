# Acceptance verification report — issue #21

Date of run: **2026-10-18**. Everything below marked ✅ was **actually
executed** on the development machine; everything that requires a real
desktop OS, a release binary, or human eyes in a real PDF viewer is
marked ⏳ with exactly what is still needed. This is the companion
evidence file for `docs/verification-plan.md` — that file's status table
points here.

## Where and how it ran

- **Machine**: headless Linux x86_64 dev machine (Ubuntu, no display).
- **Build**: branch `feat/issue-21-acceptance`, Rust 1.96.0
  (rustc ac68faa20 2026-05-25 / cargo 30a34c682 2026-05-25).
- **Driver**: `examples/verify_acceptance.rs` — a dev-time example binary
  that drives the **production pipeline**
  (`PipelineConverter::production()`: real PDFium render → real
  Tesseract OCR → real overlay write) over the committed fixtures in
  `tests/fixtures/`. No mocks anywhere in the driver path. Scratch
  output goes to `target/verify-acceptance/`; `cargo test` stays green
  and offline because the driver is an example, not a test.
- **Profile**: dev (debug). The only `debug_assert!` in the codebase is
  in `src/main.rs`, so the driver's pipeline behaves exactly like a
  release run.
- **Bundle mode**: real assets (`--cfg chilli_real_assets`), embedded
  from `assets/real/v1` (gitignored, fetched by `scripts/fetch-assets`).
  One substitution was necessary — see the disclosure below.

### Tool and asset versions used

| Component | Version / provenance |
|---|---|
| PDFium | bblanchon/pdfium-binaries pin `chromium/8100` (157.0.8100.0), linux-x64, SHA-256 verified twice (archive + inner lib) by `scripts/fetch-assets` |
| Tesseract traineddata | `tessdata_fast` @ `87416418657359cb625c412a48b6e1d6d41c29bd`, eng + hun, SHA-256 pinned, fetched by `scripts/fetch-assets` |
| Glyphless overlay font | `tests/fixtures/pdf.ttf` (572 B), copied into the bundle by `scripts/fetch-assets` |
| Tesseract engine library | **substituted** — apt `libtesseract5` 5.5.0-1build1 (`libtesseract.so.5.0.5`, 3,428,056 B) |
| Leptonica engine library | **substituted** — apt `libleptonica6` 1.86.0-1 (`libleptonica.so.6.0.0`, 2,870,032 B) |
| pdftotext (poppler-utils) | 26.01.0-2ubuntu0.2 |
| pypdf | 6.9.2 (`python3-pypdf` apt package) |
| mutool (mupdf-tools) | 1.27.0 |
| qpdf | 12.3.2-1 |
| Pillow | 12.1.1 |
| strace | 6.19 |

### ⚠ Asset substitution disclosure (must read)

`docs/packaging.md` documents that the Tesseract and Leptonica native
libraries have no canonical portable prebuilt source, so `scripts/fetch-assets`
pins **placeholder bytes** for those two slots (with a loud warning). For
this verification run the placeholders were replaced with the system
apt libraries above (same SONAMEs the placeholder slots advertise), and
the manifest was regenerated:

| Manifest entry | Size (B) | SHA-256 |
|---|---|---|
| libpdfium.so | 7,964,952 | b8784cbd652ad2993332b7695e2c80f3a797f877fc47d9be97b66bca9639539e |
| libtesseract.so | 3,428,056 | cccb7e9656f1c34d561b7acbfa257e2f49923fa73220cb2499fa3adf5c51224c |
| libleptonica.so | 2,870,032 | d5a4e9f0bbd099116ccadf8392f173c25cbc1c8011aec4c41fd37b7f2b70b910 |
| tessdata/eng.traineddata | 4,113,088 | 7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2 |
| tessdata/hun.traineddata | 5,296,273 | 35067e7cfe102dcdc953f9a758fdfaa6296b17a1ee6d874ee780fa306430b9fb |
| pdf.ttf | 572 | c7845420925a23d88ed830a63957b8af85a66a8daf8d9fc90e843673b2ef1a59 |

Consequences, stated honestly:

- The **engine behavior** (Tesseract 5.5.0 + Leptonica 1.86.0 + pinned
  tessdata) is real, and the **plumbing** (asset extraction, checksum
  verification, `dlopen` by absolute path, `tessdata_dir` resolution,
  FFI recognize) is exactly what ships. What is *not* proven here is the
  eventual pinned prebuilt Tesseract/Leptonica binaries themselves — the
  release pins will differ from apt's builds.
- The apt Tesseract links against `libcurl.so.4` and `libarchive.so.13`,
  so those libraries are present in the process during this run. They
  are never invoked by chilli-pdf (see the network observation below),
  but a release pin that avoids the curl dependency would be preferable;
  tracked in the packaging follow-up.

## Status board — acceptance checks 1–6

| # | Check | Status |
|---|-------|--------|
| 1 | Run final executable on a clean Windows machine/VM, no dev tools, no separate dependencies, networking disabled from first launch | ⏳ pending — see below |
| 2 | Convert an English scan, a Hungarian scan, and a mixed-language document | ✅ verified locally (2026-10-18, real engines) |
| 3 | Verify actual text extraction and representative search/copy results incl. `ő`, `ű` in Edge **and** ≥1 other viewer | ✅ extraction verified locally (pdftotext + pypdf); ⏳ viewer search/copy pending |
| 4 | Check alignment, appearance, page count, page dimensions, and rotation against the input | ✅ verified locally (structural + bbox + raster evidence) |
| 5 | Test mixed searchable/scanned pages, cancellation, malformed input, password-protected input, Unicode paths, output collisions, and a larger multipage document | ✅ verified locally (all sub-items; multipage at 5 pages — see note) |
| 6 | Confirm runtime processing makes no network requests | ✅ static audit (existing) + ✅ local dynamic observation (strace); ⏳ strongest form (external firewall/VM) pending |

### Check 1 — clean Windows run: ⏳ pending

Not executable here: no Windows VM, no release binary, headless Linux
only. What *was* done as the local analog: the production pipeline was
exercised from the real-asset bundle embedded in the binary —
per-user extraction to `~/.local/share/chilli-pdf/assets/v1`, checksum
verification at extraction, and `dlopen` of every engine by absolute
path from the extracted directory. Still pending, exactly as
`docs/verification-plan.md` requires: a clean offline Windows 10/11 VM,
the release `chilli-pdf.exe`, no dev tools, networking disabled from
first launch, `%LOCALAPPDATA%` extraction inspected, GUI + file dialogs
exercised by a human.

### Check 2 — eng / hun / mixed conversions: ✅ locally

Driver checks `convert: eng_scan + English`, `convert: hun_scan +
Hungarian (ő/ű)`, `convert: eng_scan + English+Hungarian preset` — all
PASS. Real OCR output decoded from the overlay of the produced files:

- **eng_scan + English** (`eng_scan_searchable.pdf`, 71,740 B):
  "Chilli PDF converts scanned documents into searchable PDFs. The
  quick brown fox jumps over the lazy dog. Searchable text can be
  selected and copied from the output. Offline OCR keeps every document
  on the local machine. Tesseract reads the page; PDFium renders it;
  the overlay aligns."
- **hun_scan + Hungarian** (`hun_scan_searchable.pdf`, 74,562 B):
  "A gyors barna róka átugrik a lusta kutya fölött. Tűzön át visz az
  út, de csak óvatosan. Őssel az őz gyakran látható a szélén az
  erdőnek. Az űrhajósok űrruhát viseltek az űrállomáson. Árvíztűrő
  tükörfúrógép."
- **eng_scan + EnglishHungarian preset**: same overlay text as the
  English run (the combined dictionary reads English correctly).

Worker event streams were asserted exactly: `Progress{1,1}` then `Done`
per single-page conversion.

### Check 3 — text extraction incl. `ő`/`ű`: extraction ✅ locally, viewers ⏳

Two independent extractors, same output file (`hun_scan_searchable.pdf`):

- **pdftotext 26.01.0**: extracted all five Hungarian lines, including
  "Az űrhajósok űrruhát viseltek az űrállomáson." and
  "Árvíztűrő tükörfúrógép." A hexdump of the extracted UTF-8 shows
  `c5 91` (ő, U+0151) and `c5 b1` (ű, U+0171) as **single precomposed
  characters** — no U+FFFD, no decomposed combining marks (U+0308 /
  U+030B), no mojibake.
- **pypdf 6.9.2**: `extract_text()` returns the same lines; scripted
  assertions: contains U+0151 ✓, contains U+0171 ✓, contains U+FFFD ✗.
- **pdftotext on the English output** likewise returns the full ground
  truth including "The quick brown fox jumps over the lazy dog."

⏳ Still pending: the same search/copy/highlight exercise in Microsoft
Edge **and** at least one other real PDF viewer, on the real machine —
by design this needs human eyes and is not automatable headlessly.

### Check 4 — alignment / appearance / page count / dimensions / rotation: ✅ locally

Four independent evidence layers, all on the driver-produced outputs:

1. **Structural (driver, lopdf)**: for every conversion — page count
   unchanged; `/MediaBox`, `/CropBox`, `/Rotate` unchanged; every
   original content stream preserved **byte-identical** (the overlay
   stream is appended, never rewrites); image streams unchanged.
   - rotated_scan: `/Rotate 90` preserved, MediaBox [0 0 792 612],
     1→2 content streams, 22-word overlay.
   - cropbox_scan: CropBox [36 36 576 756] preserved, MediaBox
     [0 0 612 792], 1→2 content streams, 29-word overlay.
2. **Word positions (pdftotext -bbox 26.01.0)**: overlay words carry
   meaningful display-space coordinates — lines share a constant yMin
   with monotonically increasing xMin. Cross-fixture control: the first
   word "This" lands at exactly **(72.000000, 75.360000)** in the eng,
   rotated, and cropbox outputs alike — matching the fixtures'
   identical 1-inch-margin generation layout. On the `/Rotate 90` page
   that means the overlay text is positioned in **rotated display
   space** (i.e. the overlay correctly compensates for the rotation
   attribute, so a viewer's search highlight lands on the visible
   word). All rotated-page word boxes lie within the 612×792 portrait
   display; all cropbox word boxes lie within the visible crop rect
   [36 36 576 756].
3. **Appearance (mutool 1.27.0 draw at 150 DPI + Pillow pixel
   compare)**: input vs. output rasterized pixel-for-pixel — zero
   differing pixels (PIL `ImageChops.difference` bounding box `None`,
   channel extrema all 0) for: eng_scan, hun_scan, rotated_scan,
   cropbox_scan (1125×1500 — the cropped visible area), gen_hybrid
   page 1 (already-text page) and page 2. The invisible overlay leaves
   the rendered appearance bit-for-bit identical.
4. **Structure (qpdf 12.3.2 `--check`)**: all nine produced outputs
   (incl. the Unicode-path and collision outputs) — exit 0, "No syntax
   or stream encoding errors found".

### Check 5 — edge cases: ✅ locally

Every sub-item driven through the production pipeline (real engines),
with exact user-facing messages captured:

- **Mixed searchable/scanned pages** (`gen_hybrid`, page 1 real text
  layer / page 2 image): PASS — after the bug fix documented below. The
  already-text page is left completely untouched (same number of
  content streams, no overlay font added — byte-identical page), the
  scanned page gets the overlay. Observable evidence: page 1 content
  stream identical, no `/ChilliOcrF1` in its resources; page 2 gains
  the overlay stream.
- **Cancellation** (`gen_multi`, 5 pages, cancel after first progress):
  events `Progress{1,5}`, `Progress{2,5}`, `Cancelled` — no output file,
  no `.chilli-pdf-tmp` residue, input intact.
- **Malformed input** (non-PDF bytes): rejected with "This file does
  not look like a valid PDF — it may be corrupted, truncated, or not a
  PDF at all. Re-save or re-export the document as a PDF and try
  again." No output, no residue.
- **Password-protected input** (`encrypted_scan.pdf`, really
  encrypted): rejected with "This PDF is password-protected
  (encrypted), which chilli-pdf cannot open yet. Remove the password
  with another PDF tool, save an unprotected copy, and try again." No
  output, no residue.
- **Digitally signed input** (synthetic signature object): rejected
  with "This PDF is digitally signed. Adding a searchable text layer
  would invalidate the signature, so chilli-pdf does not modify signed
  documents. Ask for an unsigned copy, or remove the signature
  yourself, and try again." No output, no residue.
- **Unicode paths**: input `mappák/Magyar szöveg.pdf` converts to
  `Magyar szöveg_searchable.pdf` (71,740 B) with correct OCR content.
- **Output collisions**: `check_output` reports `AlreadyExists` for a
  stale target; running the pipeline with `IfExists::Replace`
  transactionally replaces it (output starts with `%PDF`, correct
  content, no temp residue).
- **Output = input**: refused with "The chosen output location is the
  original input file. chilli-pdf never overwrites the original — pick
  a different name or folder."; input verified byte-identical
  (64,677 B) after the refusal.
- **Larger multipage document** (`gen_multi`, 5 pages): all five pages
  processed, `Progress{1..5, 5}` + `Done`, 5/5 pages carry overlay
  text. Honest scope note: five pages is a modest multipage document;
  a genuinely large (hundreds of pages) document was not exercised
  locally and remains part of the real-machine pass.

### Check 6 — no runtime network requests: partial → local ✅, strongest form ⏳

Already in place (from earlier issues, unchanged): the CI `deny` gate
(no network-capable crate linked) and the static audit in
`docs/privacy-audit.md`. New local evidence (2026-10-18): see the
strace observation below. ⏳ The strongest form — zero connection
attempts observed from outside the process (firewall / isolated VM)
with the release binary on a clean machine — still belongs to the
real-machine pass.

## Dynamic network observation (strace)

Local evidence (2026-10-18): the entire acceptance driver —
12 real OCR conversions, asset extraction, PDFium rendering, overlay
writing, cancellation — was executed under `strace 6.19` with
`strace -f -qq -e trace=network` following every thread of the process.
Result: **zero network syscalls of any kind** — not one `socket()` call,
not even an `AF_UNIX` one, and zero `connect()`/`sendto()`/`sendmsg()`
targeting anything. Combined with the CI `deny` gate (no
network-capable crate is linked) this means the complete conversion
pipeline demonstrably performs no network I/O on this machine. ⏳ The
strongest form — observation from outside the process (firewall /
isolated VM, release binary, clean machine) — still belongs to the
real-machine pass, as does the equivalent observation on Windows.

## Acceptance found a real production bug (fixed)

The first full-pipeline run of the mixed scanned/text document
(`gen_hybrid`) **failed**: sparse OCR results (text pages produce no
`PageOcrResult`) were mapped positionally into the overlay writer,
which demands exactly one result per page — so a 2-page hybrid
document died with "… has 2 pages but 1 OCR result were supplied.
Nothing was written." The committed regression test from issue #18
(`tests/integration.rs::mixed_document_overlays_only_the_scanned_page`)
was **failing on the unmodified tree**, meaning mixed scanned/text
conversion — a core product requirement — was broken in production and
the local gate had drifted red.

Fix (minimal, `src/worker.rs` only): `LopdfOverlay::apply_overlay` now
passes the page index through to the existing indexed overlay writer
(`crate::overlay::write_overlay_indexed_to`), which places each result
on its own page and leaves pages without results completely untouched.
This also removed the two dead-code warnings on the previously unused
indexed writer. `src/overlay/` was not modified; the previously failing
regression test now passes.

## Driver check results (17/17 PASS)

Run: `cargo run --example verify_acceptance` with the real-asset bundle,
dev profile, 2026-10-18, 48.7 s, exit 0.

| # | Check | Result |
|---|-------|--------|
| 1 | bundle: real assets present | PASS (manifest sizes/sha256 above) |
| 2 | convert: eng_scan + English | PASS |
| 3 | convert: hun_scan + Hungarian (ő/ű) | PASS (precomposed ő/ű, no U+FFFD) |
| 4 | convert: eng_scan + English+Hungarian preset | PASS |
| 5 | convert: gen_hybrid skips already-text page | PASS (after worker.rs fix) |
| 6 | convert: rotated_scan keeps /Rotate 90 | PASS |
| 7 | convert: cropbox_scan keeps CropBox | PASS |
| 8 | convert: gen_multi runs all five pages | PASS |
| 9 | convert: Unicode path + default output name | PASS |
| 10 | reject: encrypted scan | PASS (exact message, no residue) |
| 11 | reject: signed scan | PASS (exact message, no residue) |
| 12 | reject: malformed file | PASS (exact message, no residue) |
| 13 | reject: output == input | PASS (input unchanged) |
| 14 | collision: existing output asks, then replaces | PASS |
| 15 | cancel: mid-document leaves no trace | PASS |
| 16 | integrity: fixtures unchanged | PASS (SHA-256 of all 10 fixture PDFs equal before/after) |
| 17 | integrity: no temp residue | PASS (no `.chilli-pdf-tmp` anywhere under scratch) |

Fixture checksums verified unchanged by the run (SHA-256):
`eng_scan a22a67ab…`, `hun_scan 75c97fce…`, `rotated_scan 9b9ba5fb…`,
`cropbox_scan f7babc46…`, `encrypted_scan 566d339b…`, `gen_eng
864ab83e…`, `gen_hun 6a62d10a…`, `gen_hybrid 3d45221f…`, `gen_multi
b12bf5d4…`, `gen_rotated 5ceba3bb…` (full digests recomputed from the
committed files at report time).

## Gate results (both bundle modes)

Run on 2026-10-18 after all code changes (worker.rs fix + driver
example), with `TMPDIR` on a scratch volume, first in the default
fixture-bundle mode and then with the real-asset bundle:

- `cargo fmt --check` — **clean**
- `cargo clippy --all-targets -- -D warnings` (fixture bundle) — **0
  warnings**
- `cargo test` (fixture bundle) — **224 passed, 0 failed** (178 lib unit
  tests, 4 `fixtures_gen`, 16 `integration` — including the previously
  failing `mixed_document_overlays_only_the_scanned_page`, now green —
  3 `ocr`, 14 `overlay`, 1 `pdf_render`, 6 `preservation`, 2 doctests)
- `cargo clippy --all-targets -- -D warnings` (real-asset bundle,
  `--cfg chilli_real_assets`) — **0 warnings**

The full gate stays offline in fixture mode; the driver example is
excluded from `cargo test` by design (it needs dev-time downloads and
real OCR).

## Environment limitations (honest disclosure)

- **Headless Linux only.** No GUI launch, no file dialogs, no real PDF
  viewer, no Windows, no macOS. Every check that needs those stays ⏳.
- **Dev profile, not the release binary.** The pipeline is the
  production code path (`PipelineConverter::production()`), but LTO /
  release-packaging checks are part of the release workflow, not this
  pass.
- **OCR engine libraries substituted from apt** for the two placeholder
  pins (see the disclosure above): engine *behavior* verified, the
  eventual release *pins* not yet chosen; apt's build drags
  `libcurl.so.4` / `libarchive.so.13` into the process.
- **No user namespaces** on this machine (`unshare -rn` fails), so no
  network-namespace isolation was possible; `strace` was used for the
  dynamic network observation instead.
- **qpdf could not open paths inside the worktree** (AppArmor-style
  "Permission denied" from that prefix); outputs were copied to
  `/tmp/opencode` for `qpdf --check`. All other tools read the
  worktree directly.
- **Multipage scale**: 5 pages max locally; a large-document run
  (hundreds of pages, memory behavior under sustained load) belongs to
  the real-machine pass.

## What remains ⏳ (exactly what is needed)

1. **Clean offline Windows 10/11 VM** with the release
   `chilli-pdf.exe`: launch with networking disabled from the start, no
   dev tools, no OCR engines installed; first-launch extraction to
   `%LOCALAPPDATA%\chilli-pdf\assets\v1\`; convert a real scan; GUI
   responsiveness, file dialogs (incl. UNC paths), high-DPI.
2. **Real PDF viewer exercise**: search, select, copy in Microsoft Edge
   **and** ≥1 other viewer (e.g. Adobe Reader), including Hungarian
   `ő`/`ű` queries hitting the right highlight area on the rotated and
   cropbox pages.
3. **Strongest form of check 6**: external observation (firewall log /
   isolated VM) of zero connection attempts by the release binary.
4. **macOS / Linux desktop** runs of the issue #17 table
   (Gatekeeper, X11/Wayland sessions, Retina).
5. **Release-binary + release-pinned asset** repetition of the local
   driver checks once the real Tesseract/Leptonica pins exist and the
   release workflow produces artifacts.
