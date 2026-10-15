# Architecture (planned)

Technical design chilli-pdf converges toward. Where a decision is still
open, the corresponding issue drives a spike before committing. Nothing here
is frozen until the v0.1.0 milestone closes.

## Stack

| Concern | Choice | Notes |
|---------|--------|-------|
| Language | Rust (pinned toolchain via `rust-toolchain.toml`) | cross-compilation-friendly, memory-safe |
| GUI | `eframe`/`egui` | native, no WebView, cross-platform |
| File dialogs | `rfd` | native pickers on all three OSes |
| PDF render/parse | **PDFium** via `pdfium-render` | battle-tested renderer; loads a bundled `pdfium.dll`/`libpdfium.dylib`/`libpdfium.so` |
| OCR | **Tesseract 5** via bundled native lib + `leptonica` | eng + hun traineddata bundled |
| Text overlay | Tesseract word boxes (TSV/API) → invisible text layer (Tr 3, glyphless Type0 font, per-word rotated Tm + width-matching Tz) written in-process, merged by appending overlay ops to the original content stream | spike #6: strategy C chosen over hOCR parsing and TessPDFRenderer; original page image untouched — see docs/spike-report.md |

Alternatives considered: pure-Rust OCR (e.g. PaddleOCR-derived pipelines) —
fast-moving but noticeably weaker on Hungarian than Tesseract's mature
traineddata; `pdf`/`lopdf` writing of the overlay + content-stream append
(spike-verified) or PDFium import/stamping are both viable merge mechanisms;
the spike report records the trade-offs.

## Process layout

```
┌──────────── native window (eframe) ────────────┐
│  file picker · language combo · Convert/Cancel │
│  progress bar · output picker                   │
└──────────────┬──────────────────────────────────┘
               │ mpsc: command / progress / result
┌──────────────▼────────────────── worker thread ──────────────┐
│ 1. validate: open w/ PDFium → reject encrypted / signed /    │
│    malformed with clear errors                               │
│ 2. per page (one at a time, bounded memory):                 │
│    a. extract existing text → skip if meaningfully searchable │
│    b. render at ~300 DPI (cap for huge pages)                │
│    c. OCR (eng / hun / eng+hun) honoring rotation & cropbox  │
│    d. build invisible text layer positioned from OCR boxes   │
│ 3. stamp text layers onto original pages (content preserved) │
│ 4. write transactionally: temp file → verify → atomic rename │
└──────────────────────────────────────────────────────────────┘
```

- The worker runs page-by-page; a rendered page and its OCR result are
  dropped before the next page starts (bounded memory).
- Cancellation is cooperative: a flag checked between pages; partial output
  and temp files are deleted.
- Metadata/bookmarks/links preservation is verified per PDFium support and
  limitations documented.

## Bundling & asset sandboxing

- All native libraries, `eng.traineddata`, `hun.traineddata`, and license
  notices are embedded in the binary (`include_bytes!` / `include_dir`).
- On first run they are extracted to a **versioned per-user directory**:
  - Windows: `%LOCALAPPDATA%\chilli-pdf\<version>\`
  - macOS: `~/Library/Application Support/chilli-pdf/<version>/`
  - Linux: `$XDG_DATA_HOME/chilli-pdf/<version>/`
- A SHA-256 manifest is embedded with the assets and verified at
  extraction; mismatch → clear error, no partial load.
- Libraries are loaded **only by absolute path** from the versioned
  directory (Windows: `LoadLibraryExW` with
  `LOAD_LIBRARY_SEARCH_*` flags; never the CWD or `PATH`).
- Extraction is per-version and atomic-ish (extract to temp, rename), so a
  crashed first launch can never leave a half-written directory that later
  loads.

## Output safety

- Default output: `<original stem>_searchable.pdf` next to the input
  (`src/output_path.rs` — implemented).
- The output is refused if it resolves to the input (`check_output` —
  implemented); existing outputs trigger an explicit replace prompt.
- Write to a temp sibling, fsync, then atomic rename. Failure or cancel at
  any point deletes the temp file; the final path only ever contains a fully
  converted document.

## Offline guarantee

- `cargo deny` (or equivalent) gate in CI forbidding network-capable
  dependencies.
- No `reqwest`/`ureq`/auto-update crates anywhere in the tree.
- Verification includes a runtime network-activity check (see
  `docs/verification-plan.md`).

## Repository layout

```
src/            application + core logic (lib + bin)
docs/           requirements, architecture, verification plan
.github/        CI (3-OS matrix), release workflow, dependabot
THIRD_PARTY_NOTICES.md   licenses for everything we bundle
```
