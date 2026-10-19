# Third-party notices

chilli-pdf embeds and redistributes third-party components so the shipped
application works fully offline. This file lists everything bundled in the
distributed binaries and their licenses; it is **finalized and shipped
inside the application** at the first release. It is a living document
until then — every bundled component added by an issue must land here.

The exact bytes of each bundled component are pinned and verified by
SHA-256 at packaging time (`scripts/fetch-assets`, see `docs/packaging.md`);
the app itself never downloads anything at runtime.

| Component | Version / pinned source | License |
|-----------|------------------------|---------|
| PDFium | 157.0.8100.0 — `bblanchon/pdfium-binaries`, release `chromium/8100` (published 2026-10-12) | Apache License 2.0, plus BSD-style and other third-party portions; the release archive's `LICENSE` and `licenses/` directory list them all (abseil, agg23, dragonbox, fast_float, FreeType, HarfBuzz, ICU, lcms, libjpeg-turbo, libopenjpeg, libpng, llvm-libc, simdutf, zlib, …) |
| Tesseract OCR 5 | 5.5.3 — built from `tesseract-ocr/tesseract` source `https://github.com/tesseract-ocr/tesseract/archive/refs/tags/5.5.3.tar.gz` (SHA-256 `9218e62793116d42a9f6d14cd9348518b27f382096eea3d0f2d1a24616bb5884`), published per OS as relocated prebuilts on this repository's `ocr-prebuilts-1` release (see `docs/packaging.md`) | Apache License 2.0 |
| Leptonica (image processing used by Tesseract) | 1.87.0 — built from `DanBloomberg/leptonica` release tarball `https://github.com/DanBloomberg/leptonica/releases/download/1.87.0/leptonica-1.87.0.tar.gz` (SHA-256 `c73363397f96eb1295602bf44d708a994ad42046c791bf03ea0505d829bdb6a7`), published per OS as relocated prebuilts on this repository's `ocr-prebuilts-1` release | BSD 2-Clause (Leptonica `leptonica-license.txt`, copyright 2001–2020 Leptonica) |
| Tesseract `eng.traineddata` | `tesseract-ocr/tessdata_fast` commit `87416418657359cb625c412a48b6e1d6d41c29bd` (2024-08-01), `fast` variant | Apache License 2.0 |
| Tesseract `hun.traineddata` | `tesseract-ocr/tessdata_fast` commit `87416418657359cb625c412a48b6e1d6d41c29bd` (2024-08-01), `fast` variant | Apache License 2.0 |
| Tesseract `pdf.ttf` glyphless font | provenance `/usr/share/tesseract-ocr/5/tessdata/pdf.ttf` (the glyphless font shipped with Tesseract); the repository commits the exact production copy at `tests/fixtures/pdf.ttf` | Apache License 2.0 |
| egui / eframe | per `Cargo.lock` | MIT OR Apache-2.0 |
| rfd | per `Cargo.lock` | MIT OR Apache-2.0 |
| Rust standard library and crates | per `rust-toolchain.toml` and `Cargo.lock` | MIT OR Apache-2.0 (crate-specific) |

Full license texts for every bundled component, including all
crate-level notices, are embedded in the application at release time and
must accompany the distribution.
