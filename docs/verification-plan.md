# Verification plan

Single source of truth for what has — and has not — been verified. This
file is updated with every milestone. **No check is marked "passed" without
being actually executed**, and limitations are stated explicitly.

## Current status: core pipeline + GUI shell land; desktop-OS runtime checks pending

The repository contains a buildable, unit-tested project: the headless
conversion pipeline and the eframe/egui shell are implemented, and
`cargo test` is green on the (headless Linux) dev machine. The checks
below that can run in this development environment were executed with
real engines on 2026-10-18; the dated evidence lives in
**`docs/verification-report.md`**. Checks that need a real desktop OS, a
release binary, or human eyes in a real PDF viewer remain ⏳ and are
never assumed. Windows execution is not available in the current
development environment; any Windows-specific verification will be
performed on a clean Windows 10/11 VM and reported here, never assumed.
macOS likewise needs a real Mac.

## Acceptance checks (from product requirements)

| # | Check | Status |
|---|-------|--------|
| 1 | Run final executable on a clean Windows machine/VM, no dev tools, no separate dependencies, networking disabled from first launch | ⏳ pending — see `docs/verification-report.md` |
| 2 | Convert an English scan, a Hungarian scan, and a mixed-language document | ✅ verified locally 2026-10-18 (real engines) / ⏳ on Windows + release bundle — see `docs/verification-report.md` |
| 3 | Verify actual text extraction and representative search/copy results incl. Hungarian accents (`ő`, `ű`) in Edge **and** at least one other PDF viewer | ✅ extraction verified locally 2026-10-18 (pdftotext + pypdf) / ⏳ viewer search/copy — see `docs/verification-report.md` |
| 4 | Check alignment, appearance, page count, page dimensions, and rotation against the input | ✅ verified locally 2026-10-18 (structural + bbox + 150 DPI raster + qpdf) / ⏳ on release binary — see `docs/verification-report.md` |
| 5 | Test mixed searchable/scanned pages, cancellation, malformed input, password-protected input, Unicode paths, output collisions, and a larger multipage document | ✅ verified locally 2026-10-18 (all sub-items; multipage at 5 pages) / ⏳ larger documents + real-machine repetition — see `docs/verification-report.md` |
| 6 | Confirm runtime processing makes no network requests | ✅ static audit (CI deny gate + `docs/privacy-audit.md`) + ✅ local dynamic observation 2026-10-18 (strace) / ⏳ strongest form (external firewall/VM, release binary) — see `docs/verification-report.md` |

Check 6 (runtime network): the enforceable, already-active parts are the
CI dependency gate (`deny` job running `cargo deny check bans licenses`,
plus a `tokio/net` feature check — added by issue #20) and the static
content/network audit in `docs/privacy-audit.md`. Those prove no
network-capable crate is linked and no message path can carry document
contents, but they cannot prove the absence of OS-level connections at
runtime. The strongest form of this check — zero connection attempts
observed from outside the process (firewall rules or a network-isolated
VM), on a real machine with the release binary — belongs to the
acceptance phase (issue #21) and stays ⏳ pending until then.

## Issue #17: Cross-platform desktop integration

Everything below that is marked ✅ is proven by unit tests executed
headlessly on Linux; everything marked ⏳ requires the named real
machine and is **not** claimed as done.

### ✅ Proven locally (headless Linux, `cargo test`)

- Per-user data-root resolution for **all three OS branches**
  (`src/assets.rs::data_root_for` / `user_asset_base_dir_from`):
  `%LOCALAPPDATA%` used when absolute and ignored when missing or
  relative (Windows), no environment override (macOS),
  `$XDG_DATA_HOME` used when absolute and ignored when relative
  (Linux, per the XDG Base Directory Specification), plus the
  root + `chilli-pdf` composition for every platform.
- File-dialog configuration (`src/app.rs::FileDialogSpec`): PDF filter
  and title for both dialogs, save dialog pre-filled with the
  `<input>_searchable.pdf` name and its directory, bare-name outputs
  opening without a directory, Unicode names passed verbatim, and
  non-UTF-8 names omitted instead of lossy-mangled.
- Native-library absolute-path discipline: `library_path` returns
  absolute, canonicalized, verified paths (no `PATH`/CWD search).

### ⏳ Pending real-machine runs

| What | Where | How |
|---|---|---|
| Launch release GUI build with **no console window**; `--version` via redirection | Windows 10/11 VM | run `chilli-pdf.exe --version > out.txt` (works) and launch normally (no console flash) |
| File dialogs: PDF filter visible, Unicode paths, **network drives/UNC paths** | Windows 10/11 VM | pick/save a scan on a UNC path (`\\server\share\scan.pdf`) |
| High-DPI: no tiny/blurred window at 150%–200% scaling | Windows 10/11 VM | change display scaling, relaunch, check window and text size |
| Extraction lands in `%LOCALAPPDATA%\chilli-pdf\assets\v1\` | Windows 10/11 VM | first launch, inspect directory |
| Gatekeeper prompt for the unsigned binary; right-click → Open works; ad-hoc signature runs locally on arm64 | macOS (real Mac) | download/transfer binary (quarantined), first launch |
| High-DPI on Retina (no tiny/blurred window) | macOS (real Mac) | launch on Retina display |
| Dialogs (`NSOpenPanel`/`NSSavePanel`) incl. Unicode names | macOS (real Mac) | pick and save a scan |
| Extraction lands in `~/Library/Application Support/chilli-pdf/assets/v1/` | macOS (real Mac) | first launch, inspect directory |
| egui window + GTK file dialogs on **X11** and on **Wayland** | Linux desktop, both sessions | run in each session; dialog opens, scale factor sane |
| Full convert cycle end-to-end on each OS | all three | see acceptance checks 1–6 above |

## Issue #19: Release packaging

Everything below marked ✅ was actually executed on the dev machine
(headless Linux, network available at build time). The Tesseract and
Leptonica native libraries are still clearly-marked fixture placeholders
(see `docs/packaging.md`); everything else in the fetched set is real.

### ✅ Proven locally

- `scripts/fetch-assets` run for **all three platforms** (linux64,
  macos, windows64): each pinned PDFium archive downloaded and verified
  twice — archive SHA-256 first, then the extracted inner file — before
  placement: `libpdfium.so` (7,964,952 B), `libpdfium.dylib`
  (7,360,768 B), `pdfium.dll` (7,512,064 B), plus `tessdata_fast`
  eng/hun verified and placed and the glyphless font copied into the
  bundle.
- **Tamper test (fail-closed)**: flipping a single hex digit of a pinned
  digest makes the fetch abort with a SHA-256 mismatch error and a
  nonzero exit, placing nothing usable.
- Full gate **in both bundle modes**: `cargo fmt --check` +
  `cargo clippy --all-targets -- -D warnings` + `cargo test`
  (178 unit + 3 ocr + 14 overlay + 1 pdf_render + 6 preservation +
  2 doctests = 204 passed) is green with the committed fixture bundle
  (the default, no network) and again with the fetched linux64
  real-asset overlay embedded.
- **The mode flip is build-verified**: `build.rs` watches `assets/` via
  `cargo:rerun-if-changed`, so creating or removing the gitignored
  `assets/real/` overlay re-runs the build script and re-embeds the
  other bundle; mode-specific tests
  (`real_bundle_is_current_platform_only` /
  `fixture_bundle_carries_all_three_platforms_sets`) assert which
  bundle is actually embedded.
- Manifest regeneration stays one command in both layouts, and the
  committed `assets/manifest.txt` round-trips byte-identical
  (`cargo run --example update_asset_manifest` reports it up to date).

### ⏳ Pending release runs (CI / real machines)

| What | Where | How |
|---|---|---|
| Per-OS release job: fetch → build `--locked` → `--version` smoke-check → portable archive | GitHub Actions runners | push a `v*` tag and watch the release workflow |
| Global `SHA256SUMS` over all per-OS archives attached to the release | GitHub Actions runners | download artifacts, re-check digests |
| Release archive unpacks and runs with no dev tools, no OCR libs, no network | clean Windows/macOS/Linux machines | acceptance checks 1–6 above (issue #21) |

## Standing per-release checks

- [ ] `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings` green on all three CI platforms
- [ ] Release artifacts built by CI from the pinned real-asset overlay
      (`scripts/fetch-assets`), with a single global `SHA256SUMS`
      published alongside them
- [ ] Fetched real assets verified against pinned SHA-256 digests
      (archive and inner file) before embedding; the fetch fails closed
      on any mismatch
- [ ] Bundled asset manifest verified against embedded checksums at extraction
- [ ] No document contents in any error message or log
      (repeatable method and current results: `docs/privacy-audit.md`)
- [x] Temporary files removed after success, cancellation, and failure —
      verified locally 2026-10-18 by the acceptance driver (no
      `.chilli-pdf-tmp` residue after any path), see
      `docs/verification-report.md`
- [x] Input file byte-identical before and after a conversion (checksum) —
  asserted by every test in `tests/preservation.rs` via its `merged()` helper
  (and by `tests/overlay.rs::merged_output_preserves_the_original_appearance`)
- [x] Output file absent after a cancelled or failed conversion —
      verified locally 2026-10-18 by the acceptance driver (cancel,
      encrypted, signed, malformed, output==input paths), see
      `docs/verification-report.md`
- [ ] `THIRD_PARTY_NOTICES.md` complete and shipped with the binary

## Preservation checks verified by automated tests (issue #14)

All checks below were actually executed and pass as part of the full gate
(`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`) on the dev machine:

- [x] Metadata: the trailer's `/Info` dictionary survives with every
  entry unchanged, including UTF-16BE titles with Hungarian letters
  (`ő`, `ű`), dates, and custom keys; metadata is carried over untouched
  and `ModDate` is deliberately not bumped —
  `tests/preservation.rs::info_dict_entries_survive_the_overlay`,
  `tests/preservation.rs::rotated_page_with_metadata_survives_the_overlay`
- [x] Bookmarks/outline: a three-level outline survives with titles,
  nesting, and destinations (both `/Dest` arrays and GoTo actions)
  intact —
  `tests/preservation.rs::outline_titles_and_destinations_survive_the_overlay`
- [x] Links: internal (`/Dest` and `/A` GoTo) and external (`/URI`)
  link annotations keep their object ids, hit rectangles, and targets;
  the overlay adds no annotations —
  `tests/preservation.rs::internal_and_external_links_survive_the_overlay`
- [x] Page tree: page count, object ids, `/Kids` order, `/MediaBox`,
  `/CropBox`, and `/Rotate` (including a 90°-rotated page) are
  unchanged; the overlay stream is appended to `/Contents` —
  `tests/preservation.rs::page_tree_and_order_are_unchanged_for_every_fixture`,
  `tests/overlay.rs::merged_output_preserves_the_original_appearance`
- [x] Existing page font resources are kept; the OCR font is added
  alongside them —
  `tests/preservation.rs::existing_font_resources_are_kept_beside_the_overlay_font`
- [x] Documents whose cross-reference data requires last-resort repair
  are refused as unreadable instead of being edited through a guessed
  xref table —
  `tests/overlay.rs::input_needing_xref_repair_is_refused_as_unreadable`,
  `src/worker.rs` unit test `lopdf_overlay_maps_unreadable_input_to_overlay_error`

## Environment limitations to disclose in the release report

- Where Windows/macOS/Linux execution was unavailable (e.g. no Windows VM),
  the report must say so explicitly and list which checks remain pending.
