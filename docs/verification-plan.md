# Verification plan

Single source of truth for what has — and has not — been verified. This
file is updated with every milestone. **No check is marked "passed" without
being actually executed**, and limitations are stated explicitly.

## Current status: core pipeline + GUI shell land; desktop-OS runtime checks pending

The repository contains a buildable, unit-tested project: the headless
conversion pipeline and the eframe/egui shell are implemented, and
`cargo test` is green on the (headless Linux) dev machine. **None of the
acceptance checks below have been run on a real desktop OS yet.**
Windows execution is not available in the current development
environment; any Windows-specific verification will be performed on a
clean Windows 10/11 VM and reported here, never assumed. macOS likewise
needs a real Mac.

## Acceptance checks (from product requirements)

| # | Check | Status |
|---|-------|--------|
| 1 | Run final executable on a clean Windows machine/VM, no dev tools, no separate dependencies, networking disabled from first launch | ⏳ pending |
| 2 | Convert an English scan, a Hungarian scan, and a mixed-language document | ⏳ pending |
| 3 | Verify actual text extraction and representative search/copy results incl. Hungarian accents (`ő`, `ű`) in Edge **and** at least one other PDF viewer | ⏳ pending |
| 4 | Check alignment, appearance, page count, page dimensions, and rotation against the input | ⏳ pending |
| 5 | Test mixed searchable/scanned pages, cancellation, malformed input, password-protected input, Unicode paths, output collisions, and a larger multipage document | ⏳ pending |
| 6 | Confirm runtime processing makes no network requests | ⏳ pending |

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

## Standing per-release checks

- [ ] `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings` green on all three CI platforms
- [ ] Release artifacts built by CI with SHA-256 checksums published
- [ ] Bundled asset manifest verified against embedded checksums at extraction
- [ ] No document contents in any error message or log
- [ ] Temporary files removed after success, cancellation, and failure
- [ ] Input file byte-identical before and after a conversion (checksum)
- [ ] Output file absent after a cancelled or failed conversion
- [ ] `THIRD_PARTY_NOTICES.md` complete and shipped with the binary

## Environment limitations to disclose in the release report

- Where Windows/macOS/Linux execution was unavailable (e.g. no Windows VM),
  the report must say so explicitly and list which checks remain pending.
