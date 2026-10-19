# Packaging (issue #19)

How release binaries get their real, bundled assets — PDFium,
Tesseract, Leptonica, the `eng`/`hun` traineddata models, and the
glyphless font — and how those artifacts are pinned and verified.

## Build-time vs runtime network use

chilli-pdf **never touches the network at runtime**. All downloading
happens at **packaging time**, inside `scripts/fetch-assets`, on the
machine that builds a release (a CI runner or a maintainer's machine).
The downloaded bytes are embedded into the binary at compile time;
end-user machines make zero network requests. This distinction is part
of the offline guarantee in `docs/privacy-audit.md`: the fetch script
and `build.rs` are build-time tooling and are not part of the shipped
binary's behavior; the app's dependency tree stays network-free under
`cargo deny`.

## The two bundles

`src/assets.rs` embeds exactly one asset bundle into the binary, chosen
by `build.rs`:

| Mode | When | Bundle | Contents |
|------|------|--------|----------|
| **fixture** (default) | no `assets/real/` present | `assets/v1/` + `assets/manifest.txt` (committed) | tiny placeholder blobs for **all three platforms** + the real `pdf.ttf` — development needs no network and no fetch |
| **real assets** (`--cfg chilli_real_assets`) | `scripts/fetch-assets` has populated `assets/real/` | `assets/real/v1/` + `assets/real/manifest.txt` (gitignored) | the pinned, checksum-verified real set for **exactly one platform** |

Both bundles use the same generated manifest format and the same
verification code path, so extraction, fail-closed checks, and tests are
identical in both modes. `rm -rf assets/real` returns a checkout to
fixture mode.

## Per-OS asset set

Every shipped build bundles exactly these files for its platform:

| File | Windows | macOS | Linux |
|------|---------|-------|-------|
| PDFium | `pdfium.dll` | `libpdfium.dylib` | `libpdfium.so` |
| Tesseract | `tesseract.dll` | `libtesseract.dylib` | `libtesseract.so` |
| Leptonica | `leptonica.dll` | `libleptonica.dylib` | `libleptonica.so` |
| English model | `tessdata/eng.traineddata` (same for all) | | |
| Hungarian model | `tessdata/hun.traineddata` (same for all) | | |
| Glyphless font | `pdf.ttf` (same for all) | | |

The unit tests in `src/assets.rs` assert the current platform's full
set in **both** modes, that the fixture bundle additionally carries all
three platforms (so dev builds are platform-independent), and that a
fetched real bundle contains no foreign-platform files.

## Pins and the fetch flow

All pins live in the pin table at the top of `scripts/fetch-assets`:

- **PDFium** — `bblanchon/pdfium-binaries`, release tag `chromium/8100`
  (PDFium 157.0.8100.0, published 2026-10-12). Both the downloaded
  archive **and** the library inside it are verified against pinned
  SHA-256 values before the library is placed into the overlay.
- **traineddata** — `tesseract-ocr/tessdata_fast` at commit
  `87416418657359cb625c412a48b6e1d6d41c29bd` (2024-08-01), `eng` and
  `hun`, each verified against its pinned SHA-256. The `fast` variants
  are the size/speed sweet spot for interactive use.
- **Glyphless font** — the repository commits the exact production font
  (`tests/fixtures/pdf.ttf`, provenance
  `/usr/share/tesseract-ocr/5/tessdata/pdf.ttf`); the fetch script
  copies it into the overlay rather than downloading it.
- **macOS** — the pins are the `mac-arm64` artifacts because the
  release workflow builds on `macos-latest` (Apple Silicon) runners.
  Intel macs are out of scope for v1.

The script is fail-closed end to end: any checksum mismatch aborts
before anything reaches the overlay, and the overlay is wiped and
rebuilt from scratch on every run, so re-runs are deterministic and no
stale or foreign files can leak into a build. After placing the files
it regenerates `assets/real/manifest.txt` with the same canonical
generator as the fixture bundle:

```sh
cargo run --example update_asset_manifest -- --dir assets/real/v1 --out assets/real/manifest.txt
```

### Tesseract & Leptonica native libraries: PLACEHOLDER

There is **no canonical portable prebuilt source** for the Tesseract and
Leptonica libraries across the three OSes:

- Windows: the UB-Mannheim build is the de-facto standard, but ships as
  an installer, not as standalone pinned library files.
- Linux: distro packages are not vendorable (sonames/paths differ per
  distro).
- macOS: Homebrew bottles are built against the local prefix and are
  not portable.

Chosen strategy (decision recorded in issue #19): **vendored prebuilts,
pinned per release exactly like PDFium** — i.e. pin a stable upstream
artifact per OS, verify it, embed it. Until such artifacts are picked,
`scripts/fetch-assets` marks these two slots as **placeholders**: it
copies the committed fixture blobs into the real overlay and prints a
loud warning, so the manifest stays complete, checksums stay honest,
and any release built from the overlay is visibly incapable of real
OCR (see `THIRD_PARTY_NOTICES.md`). Candidate sources to evaluate when
replacing the placeholder: extracting `tesseract.dll`/`leptonica.dll`
from a pinned UB-Mannheim build (Windows), and producing static or
relocated prebuilts in our own CI from pinned upstream source releases
(Linux/macOS).

### The placeholder guard (issue #42) — fail-closed by profile and by bytes

v1.0.0 shipped a release binary whose Tesseract/Leptonica slots held
the fixture placeholder bytes: manifest checksum verification passed
because the placeholder checksums were themselves pinned. `build.rs`
now refuses to let that happen again. The canonical placeholder
definition is the committed fixture manifest (`assets/manifest.txt`):
the six Tesseract/Leptonica entries there **are** the placeholder bytes
by definition, so the guard stays correct automatically whenever the
fixtures are regenerated. Guard semantics — no bypass, no escape-hatch
environment variable:

| Situation | Result |
|-----------|--------|
| fixture bundle + `cargo build --release` (any non-debug profile) | **hard build error** |
| fixture bundle + debug profile (`cargo test`, `cargo clippy`, dev builds) | allowed as a clearly-labeled **DEGRADED build** with a loud `cargo:warning` |
| `assets/real/` present + any Tesseract/Leptonica file byte-identical to a fixture placeholder | **hard build error, every profile** |

Practical consequences:

- `cargo test` keeps working offline from a fresh checkout, in fixture
  mode — the whole unit suite is built on the placeholder blobs and
  the DEGRADED warning states exactly what the binary contains.
- `cargo build --release` **requires** `scripts/fetch-assets` to have
  run first. The CI `build` job therefore builds the debug profile
  (fixture mode); release-profile compile coverage lives in the `e2e`
  CI job (real assets fetched at build time) and in the Release
  workflow (which always fetches assets before building).
- While the Tesseract/Leptonica pins are still placeholders (see the
  section above), any release-profile build against a fetched overlay
  fails: the fetch script copies the placeholder bytes for those two
  slots, and the guard refuses them even in real-assets mode. That is
  the guard doing its job — a release binary with placeholder engines
  must never exist again. Real pins turn this green without any
  further change.
- A missing `PROFILE` variable or an unreadable/unparseable fixture
  manifest is also a hard error: the guard fails closed on incomplete
  data rather than silently skipping the check.

`tests/build_guard.rs` pins the guard's data source: the fixture
manifest must always list all six OCR library slots with well-formed
SHA-256 values and placeholder-sized files.

## Re-pinning procedure

1. Bump the pin values in `scripts/fetch-assets` (URLs + both SHA-256
   columns: archive **and** inner library). Never widen verification.
2. Run `scripts/fetch-assets` for every platform and let the checksum
   gate pass (a mismatch means either a wrong pin or a compromised
   source — investigate, do not bypass).
3. Bump `ASSET_VERSION` in `src/assets.rs` whenever any pinned asset's
   bytes change, so existing users get a fresh extraction directory
   instead of a checksum failure against the previously published set.
4. Update `THIRD_PARTY_NOTICES.md` (versions and sources) and this
   file's pin references.
5. Ship through the Release workflow only — never commit fetched
   binaries; `assets/real/` is gitignored.

## Release workflow

`.github/workflows/release.yml` (on `v*` tags) runs per OS:

checkout → toolchain → `scripts/fetch-assets --platform <os>` →
`cargo build --release --locked` → headless smoke check
(`<binary> --version`, which exits before any GUI or asset work) →
package a portable archive (zip on Windows, tar.gz elsewhere) holding
the binary, `LICENSE`, `THIRD_PARTY_NOTICES.md`, and the short offline
`README.txt` from `packaging/` → upload.

The release job then merges the per-OS archives and publishes one
global `SHA256SUMS` covering all of them, attaching everything to the
GitHub release via `softprops/action-gh-release@v3`. Acceptance for an
archive: unpack on a machine with no dev tools, no OCR libraries, and
no network — and it runs.
