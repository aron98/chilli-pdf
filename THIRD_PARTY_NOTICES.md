# Third-party notices

chilli-pdf embeds and redistributes third-party components so the shipped
application works fully offline. This file lists everything bundled in the
distributed binaries and their licenses; it is **finalized and shipped
inside the application** at the first release. It is a living document
until then — every bundled component added by an issue must land here.

| Component | Purpose | License |
|-----------|---------|---------|
| PDFium | PDF rendering, page manipulation | Apache License 2.0 (plus BSD-style portions, see PDFium's `LICENSE`) |
| Tesseract OCR 5 | optical character recognition | Apache License 2.0 |
| Leptonica | image processing used by Tesseract | BSD 2-Clause |
| Tesseract `eng.traineddata` | English language model | Apache License 2.0 |
| Tesseract `hun.traineddata` | Hungarian language model | Apache License 2.0 |
| Tesseract `pdf.ttf` glyphless font | invisible-text layer for searchable PDF output | Apache License 2.0 |
| egui / eframe | native GUI toolkit | MIT OR Apache-2.0 |
| rfd | native file dialogs | MIT OR Apache-2.0 |
| Rust standard library and crates per `Cargo.lock` | toolchain and dependencies | MIT OR Apache-2.0 (crate-specific) |

Full license texts for every bundled component, including all
crate-level notices, are embedded in the application at release time and
must accompany the distribution.
