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

- All native libraries (PDFium, Tesseract, Leptonica), the
  `eng.traineddata`/`hun.traineddata` models, the glyphless overlay font,
  and license notices are embedded in the binary (`include_dir!` over a
  bundle directory, described by its generated SHA-256 manifest).
- There are two bundles (see `docs/packaging.md`): the committed fixture
  bundle `assets/v1/` (tiny placeholder blobs for all platforms — the
  default, so development needs no network), and the gitignored
  real-asset overlay `assets/real/v1/` populated by the pinned,
  checksum-verifying `scripts/fetch-assets` step (issue #19). `build.rs`
  selects the overlay when it exists (`--cfg chilli_real_assets`).
- Implementation lives in `src/assets.rs`; the layout and names below are
  its API. Re-pinning the real assets is **data-only**: update the pins in
  `scripts/fetch-assets`, rerun it, bump `ASSET_VERSION` (and regenerate
  `assets/manifest.txt` via `cargo run --example update_asset_manifest`
  after any fixture change).
- On first run assets are extracted to a **versioned per-user directory**:
  - Windows: `%LOCALAPPDATA%\chilli-pdf\assets\v<ASSET_VERSION>\`
  - macOS: `~/Library/Application Support/chilli-pdf/assets/v<ASSET_VERSION>/`
  - Linux: `$XDG_DATA_HOME/chilli-pdf/assets/v<ASSET_VERSION>/`
  (the data root is resolved by the pure `data_root_for` helper in
  `src/assets.rs`: home directory via the `dirs` crate plus each OS's
  convention — `%LOCALAPPDATA%` override on Windows, `$XDG_DATA_HOME`
  override, absolute values only, on Linux — so every branch is
  unit-testable on any host)
- A SHA-256 manifest is embedded with the assets (`AssetManifest`). Every
  file is verified on load: written temp files are checked before publish,
  an existing version directory is re-verified on every start, and a
  mismatch fails closed (`AssetError::ChecksumMismatch`) — never a silent
  load of an unverifiable asset.
- Extraction is transactional (`ensure_extracted`): write to a sibling
  temp dir `.tmp-extract-v<version>-*`, verify, then atomic rename. A
  crashed first launch can only leave a temp dir — never a half-written
  directory the app would treat as valid — and stale temp dirs are
  cleaned on the next run.
- Versions are isolated: `v1` and `v2` directories coexist, and extraction
  never mutates or deletes another version's files.
- Libraries are loaded **only by absolute path** from the verified
  directory (`ExtractedAssets::library_path` for `NativeLib::Pdfium` /
  `NativeLib::Tesseract` / `NativeLib::Leptonica`; Leptonica is handed
  to the loader before Tesseract, dependencies first, per the
  `OcrPaths` contract in `src/ocr/paths.rs`; Windows:
  `LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_*` flags — see the known
  gap in `docs/platform-notes.md`; never the CWD, `PATH`, or any system
  search path — a tampered library planted in a search path must never
  be picked up instead of the verified one).

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
assets/         embedded bundle (assets/v1) + generated SHA-256 manifest
docs/           requirements, architecture, verification plan
.github/        CI (3-OS matrix), release workflow, dependabot
THIRD_PARTY_NOTICES.md   licenses for everything we bundle
```
