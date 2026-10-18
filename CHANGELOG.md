# Changelog

All notable changes to chilli-pdf are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning
follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [1.0.0] - 2026-10-18

First stable release: the complete, fully offline scanned-PDF →
searchable-PDF converter, end to end.

### Added

- Native GUI conversion pipeline (eframe/egui with native file dialogs):
  pick a scanned PDF, choose English, Hungarian, or English + Hungarian
  recognition, and convert — one document at a time, with per-page
  progress and cancellation at any point.
- ~300 DPI page rendering for recognition (PDFium), with a pixel-budget
  cap that bounds memory on unusually large pages.
- Invisible Unicode text overlay: recognized words are added as an
  invisible layer aligned on top of the original page, so appearance,
  rotation, crop boxes, bookmarks, links, and metadata are preserved —
  page content is never re-rasterized. Hungarian `ő`/`ű` survive as
  precomposed characters in extraction and search.
- Pages that already contain meaningful extractable text are skipped
  completely untouched.
- Input validation with clear, content-free messages: encrypted
  (password-protected), digitally signed, and malformed PDFs are
  refused up front with actionable guidance; documents whose
  cross-reference data needs repair are refused rather than edited
  through a guessed xref table.
- Transactional output: default output name `<name>_searchable.pdf`,
  temp-file plus atomic-rename publish, an explicit prompt before
  replacing an existing file, a hard refusal when output = input, and
  no temporary residue after success, cancellation, or failure.
- Versioned, checksum-verified per-user asset extraction: the PDFium,
  Tesseract, and Leptonica libraries and the eng/hun traineddata models
  are embedded in the binary, extracted into a versioned per-user
  directory on first launch, verified against a SHA-256 manifest, and
  loaded only by absolute path from that directory.
- Portable offline release artifacts per OS (Windows x64 zip, macOS
  tar.gz, Linux x64 tar.gz), built by the Release workflow from pinned,
  checksum-verified assets with a global `SHA256SUMS` —
  see `docs/packaging.md`.
- Offline/privacy CI gate: `cargo deny check bans licenses` (no
  network-capable crate may link), a `tokio/net` feature check, and a
  content-free error-message audit — see `docs/privacy-audit.md`.
- Integration and preservation test suites (224 tests) covering the
  end-to-end pipeline, overlay geometry, metadata/bookmark/link/font
  preservation, byte-identical inputs, and appearance-identical output.
- Acceptance verification (`docs/verification-report.md`): 17/17
  driver checks passed locally with the real engines on 2026-10-18,
  including a zero-network-syscall run under `strace`; the checks that
  require a clean offline Windows VM, real PDF viewers (search/copy in
  Edge and others), or the release binary remain explicitly
  pending-real-machine and are never claimed as verified.
- Documentation set: product requirements, architecture, packaging,
  platform notes, preservation summary, privacy audit, verification
  plan, and the honest verification report.

### Fixed

- Mixed scanned/text documents: sparse OCR results (already-text pages
  produce no OCR result) are now mapped to their own page indexes, so
  a hybrid document converts correctly instead of failing with a page
  count mismatch. Found by the acceptance driver; pinned by a
  regression test.

### Known limitations

- Partially searchable pages are out of scope: a page either already
  carries meaningful text (skipped) or is scanned (gets the OCR overlay).
- Signed and encrypted (password-protected) PDFs are refused in v1;
  they must be unsigned/unprotected copies.
- OCR quality is bounded by Tesseract and the bundled `tessdata_fast`
  models; handwriting recognition and table reconstruction are out of
  scope.
- No batch mode: one document at a time.
