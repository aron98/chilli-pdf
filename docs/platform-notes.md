# Platform notes

How chilli-pdf behaves on Windows, macOS, and Linux: what the code does
per OS, and — critically — what has actually been verified where. Checks
marked **⏳** have not been executed on that OS yet and are tracked in
`docs/verification-plan.md`; nothing here claims an unverified result.

Scope note: the development machine for this work is headless Linux, so
everything that could be proven locally (pure unit tests, headless
`cargo test`) is proven there; per-OS runtime behavior below is derived
from the code and the upstream documentation of `eframe`/`winit`/`rfd`
and must still be confirmed on real machines.

## Per-OS behavior table

| Concern | Windows | macOS | Linux |
|---|---|---|---|
| Per-user asset dir | `%LOCALAPPDATA%\chilli-pdf\assets\v<n>\` | `~/Library/Application Support/chilli-pdf/assets/v<n>/` | `$XDG_DATA_HOME/chilli-pdf/assets/v<n>/` (default `~/.local/share/chilli-pdf/assets/v<n>/`) |
| Data-root resolution | `%LOCALAPPDATA%` when absolute, else `<home>\AppData\Local` | `~/Library/Application Support` (no env override in the convention) | `$XDG_DATA_HOME` when absolute; relative values ignored per the XDG Base Directory Specification |
| Home directory source | `dirs::home_dir` (Known Folder / profile) | `dirs::home_dir` (pwent) | `dirs::home_dir` (`$HOME`, pwent fallback) |
| File dialog backend | `rfd` → Common Item Dialog (`IFileOpenDialog` / `IFileSaveDialog`, COM) | `rfd` → `NSOpenPanel` / `NSSavePanel` | `rfd` → GTK 3 file chooser |
| Dialog pre-fill | PDF filter, suggested `<input>_searchable.pdf` name, output's directory | same | same |
| DPI scaling | OS scale factor applied by `winit`/`eframe`; all layout in logical points | Retina backing scale factor applied automatically | Wayland/X11 scale factor applied; layout in logical points |
| Console window | none in release GUI builds (see below) | n/a | n/a |
| Gatekeeper / code signing | Authenticode out of scope for v1 | unsigned binary → Gatekeeper prompt (see below); notarization post-v1 | n/a |
| Display servers | n/a | n/a | X11 and Wayland both supported by `eframe`/`winit` |

## Per-user asset directories

Implemented in `src/assets.rs`:

- `data_root_for(platform, home, env)` — pure per-OS root resolution
  (the table above), taking an injected environment lookup so every OS
  branch is unit-tested on any host.
- `user_asset_base_dir_from(...)` — composes root + `chilli-pdf`.
- `user_asset_base_dir()` — the production call: `dirs::home_dir()` plus
  real `std::env::var_os` values, resolving exactly as the pure helpers.
- Extraction is per-user and needs no admin rights; nothing is written
  outside the per-user directory (and the user-chosen output location).

✅ Unit-tested on Linux for all three OS branches, including absolute
env overrides, relative (invalid) overrides, and missing overrides.
⏳ Real extraction on Windows/macOS must be observed on those machines.

## File dialogs

Implemented in `src/app.rs` as pure `FileDialogSpec` data
(`input_dialog_spec()` / `output_dialog_spec()`), applied to `rfd` at the
last moment:

- Filter: "PDF document" (`*.pdf`) on both open and save dialogs.
- Save dialog pre-fills the suggested name from
  `output_path::default_output_path` (`<input>_searchable.pdf`) and the
  output's directory; the open dialog pre-fills nothing (the OS
  remembers the last location).
- Unicode paths: the suggested file name is passed through `to_str()`
  verbatim; a name that is not valid Unicode is omitted rather than
  mangled by a lossy conversion. Picked paths are `PathBuf`s, so
  non-UTF-8 input/output paths work unchanged.

✅ Spec construction is unit-tested headlessly (including the non-UTF-8
case). ⏳ Opening the actual dialog (filters visible, Unicode names in
the UI, Windows network drives/UNC paths, macOS panels) needs a real
desktop on each OS.

## DPI scaling

The app does not hardcode pixel sizes or scale factors:

- Window size is set with `ViewportBuilder::with_inner_size`, which is
  in **logical points** (`eframe`/`winit` multiply by the OS scale
  factor), and everything in the UI is laid out in egui logical points.
- The app never calls `set_pixels_per_point` and never reads physical
  pixels, so per-monitor Windows scaling, macOS Retina backing stores,
  and Linux Wayland/X11 scale factors are honored by the toolkit.

✅ No hardcoded scale anywhere in the code (inspectable). ⏳ Visual
checks (no tiny or blurred window at 150–200% on Windows, Retina macOS,
fractional Wayland scaling) need real displays.

## Windows: console subsystem

`src/main.rs` carries:

```rust
#![cfg_attr(
    all(target_os = "windows", feature = "gui", not(debug_assertions)),
    windows_subsystem = "windows"
)]
```

- Release + `gui` feature: GUI subsystem → **no console window** flashes
  when the app launches. `--version`/`--help` still print correctly when
  stdout is redirected (scripts, CI, `>` redirection); they are not
  shown in an interactive console, as usual for GUI-subsystem binaries.
- Debug builds and `--no-default-features` headless builds keep the
  console, so `cargo test`/CLI workflows on Windows behave normally.
- Other platforms are unaffected (target-gated).

✅ Attribute verified to compile-gate correctly on Linux (it is inert
here). ⏳ Behavior (launch without console) must be observed on a
Windows VM.

## macOS: Gatekeeper

The v1 binary is **unsigned and un-notarized**:

- A binary downloaded from the internet is quarantined; first launch
  shows Gatekeeper's "cannot be opened because Apple cannot check it
  for malicious software" (or "from an unidentified developer").
- Workarounds users can apply today: right-click → Open, or
  System Settings → Privacy & Security → Open Anyway, or
  `xattr -d com.apple.quarantine <binary>` for scripted installs.
- On Apple Silicon the linker applies an ad-hoc signature, which is
  enough to run locally but does not clear quarantine for downloaded
  files.
- **Full notarization is explicitly post-v1** (requires an Apple
  Developer account and CI signing); this document will be updated when
  that lands.

⏳ All macOS behavior — needs a real Mac (see verification plan).

## Linux: Wayland and X11

- `eframe`/`winit` support both display servers; the app uses no
  X11-specific APIs. `rfd`'s GTK 3 dialogs work on both, natively under
  Wayland (GTK has its own Wayland backend).
- No compositor-specific workarounds are applied; if a distro ships a
  broken GTK theming setup, that is a system issue, not app code.

⏳ Runtime confirmation in both a Wayland and an X11 session is pending
(headless dev machine has no display server at all).

## Native library loading

- PDFium and Tesseract are loaded **only by absolute, canonicalized
  paths** inside the verified per-user asset directory
  (`ExtractedAssets::library_path`); `src/pdf/render.rs` rejects
  non-absolute paths before binding, and no `PATH`/`CDLL` search is
  used on any platform.
- Known gap (out of scope for issue #17, tracked as a follow-up):
  on Windows the loader call goes through `libloading`'s
  `Library::new`, which maps to `LoadLibraryExW` with flags `0` — i.e.
  the default DLL search order still applies to the *dependencies* of
  the verified library. `docs/architecture.md` prescribes
  `LOAD_LIBRARY_SEARCH_*` flags; adopting
  `libloading::os::windows::Library::load_with_flags` at the call sites
  closes this and will be done with the real Windows binaries (the
  current fixtures have no dependent DLLs).

✅ Absolute-path discipline is unit-tested on Linux. ⏳ Real library
loading on Windows/macOS needs those machines.
