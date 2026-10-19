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
- **Tesseract 5.5.3 + Leptonica 1.87.0** — our own relocated prebuilts,
  built from the pinned upstream sources by
  `.github/workflows/native-libs.yml` and published on this repository's
  `ocr-prebuilts-1` release. As with PDFium, both the downloaded archive
  **and** the library inside it are verified against pinned SHA-256
  values (see the prebuilts section below).
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

### Tesseract & Leptonica native libraries: our own prebuilts

There is **no canonical portable prebuilt source** for the Tesseract and
Leptonica libraries across the three OSes:

- Windows: the UB-Mannheim build is the de-facto standard, but ships as
  an installer, not as standalone pinned library files (details on why
  it is unsuitable below).
- Linux: distro packages are not vendorable (sonames/paths differ per
  distro).
- macOS: Homebrew bottles are built against the local prefix and are
  not portable.

Chosen strategy (decision recorded in issues #19 and #42): we build
**our own relocated prebuilts from pinned upstream source releases**
and publish them as stable artifacts attached to a dedicated non-`v*`
release tag of this repository (`ocr-prebuilts-1`), exactly the model
`bblanchon/pdfium-binaries` provides for PDFium. `scripts/fetch-assets`
pins each platform archive's URL plus the SHA-256 of **both** the
archive and the inner library, and verifies both before anything
reaches the overlay.

Pinned upstream sources (also recorded in the pin table and the
`ocr-prebuilts-1` release notes):

| Source | Version | URL | SHA-256 |
|--------|---------|-----|---------|
| Leptonica | 1.87.0 (2025-12-24) | `https://github.com/DanBloomberg/leptonica/releases/download/1.87.0/leptonica-1.87.0.tar.gz` | `c73363397f96eb1295602bf44d708a994ad42046c791bf03ea0505d829bdb6a7` |
| Tesseract | 5.5.3 (2026-07-24) | `https://github.com/tesseract-ocr/tesseract/archive/refs/tags/5.5.3.tar.gz` | `9218e62793116d42a9f6d14cd9348518b27f382096eea3d0f2d1a24616bb5884` |

`.github/workflows/native-libs.yml` (`workflow_dispatch`) builds all
three platforms from exactly these pins:

| Platform | Runner | Build configuration | Relocation contract |
|----------|--------|---------------------|----------------------|
| linux64 | `ubuntu:22.04` container on an `ubuntu-latest` runner | leptonica: all codecs off (chilli feeds raw pixels; no codec paths exist), shared only, no programs; tesseract: no training tools, graphics disabled, OpenMP off, archive/curl/TIFF off | soname set to `libleptonica.so`, tesseract rpath `$ORIGIN` — resolves the sibling from the extracted per-user assets dir; glibc ≥ 2.35 baseline (Ubuntu 22.04 / Debian 12 era), asserted in the workflow |
| macos | `macos-15` (arm64, matches the PDFium mac-arm64 pin) | same | install name `@loader_path/libleptonica.dylib`, ad-hoc re-signed after editing |
| windows64 | `windows-latest` (MSVC) | same feature set, static CRT (`MultiThreaded`) so no `vcruntime140.dll` dependency | outputs exactly `tesseract.dll` + `leptonica.dll`; tesseract imports `leptonica.dll` **by name**, satisfied by the leptonica-first preload in `src/ocr/ffi.rs` — no Windows loader search-path dependence |

Each workflow job ends with hard assertions on the produced binaries
(ELF `DT_NEEDED`/`DT_RUNPATH`, Mach-O load commands + code signature,
PE import tables), so no absolute build-machine path can ever ship and
the Windows pair can never grow a non-system import.

#### Why Windows does not use the UB-Mannheim installer (issue #42)

The original plan — extract `tesseract.dll`/`leptonica.dll` from a
pinned UB-Mannheim installer — was investigated and rejected on the
following evidence, verified against the actual installers:

- The installer is an **NSIS** package (Nullsoft v3.08), not Inno
  Setup, so `innoextract` cannot open it at all (it supports Inno
  Setup ≤ 6.0.5).
- The libraries inside are `libtesseract-5.dll` and
  `libleptonica-6.dll`, not the bundled slot names; renaming files
  afterwards does not rename the PE import entries that reference them.
- Those import entries are **eager (static) imports** — the delay-load
  directory is empty — of a ~32-DLL MinGW closure totalling ~46 MB,
  including `libcurl-4.dll`, `libcrypto-3-x64.dll`, `libarchive-13.dll`
  and eight codec DLLs. Bundling a network/TLS stack in a
  privacy-focused offline app is unacceptable, and the closure would
  double-load leptonica under two names.

Building our own minimal pair from the same pinned sources keeps the
two-DLL bundle, the static CRT, the no-network property, and the
leptonica-first preload resolution contract documented above.

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
  (fixture mode); release-profile compile coverage lives in the
  Release workflow (which always fetches assets before building).
- With the real Tesseract/Leptonica pins in place (see the prebuilts
  section above), a fetched overlay passes the guard in every profile:
  the six OCR slots hold real engines whose digests differ from the
  fixture placeholders. The guard stays as the permanent tripwire — if
  the pins ever regress to placeholder bytes, the release build fails
  again.
- A missing `PROFILE` variable or an unreadable/unparseable fixture
  manifest is also a hard error: the guard fails closed on incomplete
  data rather than silently skipping the check.

`tests/build_guard.rs` pins the guard's data source: the fixture
manifest must always list all six OCR library slots with well-formed
SHA-256 values and placeholder-sized files.

## Re-pinning procedure

For the Tesseract/Leptonica slots, re-pinning starts one step earlier —
produce new prebuilts first:

1. Update the source pins (URLs + SHA-256) in both
   `scripts/fetch-assets` **and** `.github/workflows/native-libs.yml`
   (they must stay in sync — the workflow builds what the pin table
   claims), then run the workflow via `workflow_dispatch`.
2. Verify the built binaries' relocation assertions pass (the workflow
   enforces them), and publish the three archives on a new dedicated
   non-`v*` release tag (e.g. `ocr-prebuilts-2`) with the source pins
   recorded in the release notes.
3. Bump the pin values in `scripts/fetch-assets` (URLs + both SHA-256
   columns: archive **and** inner library). Never widen verification.
4. Run `scripts/fetch-assets` for every platform and let the checksum
   gate pass (a mismatch means either a wrong pin or a compromised
   source — investigate, do not bypass).
5. Bump `ASSET_VERSION` in `src/assets.rs` whenever any pinned asset's
   bytes change, so existing users get a fresh extraction directory
   instead of a checksum failure against the previously published set.
6. Update `THIRD_PARTY_NOTICES.md` (versions and sources) and this
   file's pin references.
7. Ship through the Release workflow only — never commit fetched
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
