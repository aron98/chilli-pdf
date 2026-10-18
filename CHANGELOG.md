# Changelog

All notable changes to chilli-pdf are documented here. The format is based
on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning
follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Project scaffold: cross-platform Rust workspace (lib + bin) that builds and
  tests on Windows, macOS, and Linux.
- Core logic with unit tests: default output naming
  (`<name>_searchable.pdf`) and output-conflict detection (never overwrite
  the input, prompt before replacing existing output).
- OCR language presets (English, Hungarian, English + Hungarian) with the
  Tesseract language codes they will bundle.
- CI matrix (`ubuntu-latest`, `macos-latest`, `windows-latest`) with
  fmt/clippy/test gates, plus a release workflow for `v*` tags with
  SHA-256 checksums.
- Pinned, checksum-verifying release-asset pipeline (issue #19):
  `scripts/fetch-assets` downloads the pinned PDFium, tessdata models,
  and (as marked placeholders) Tesseract/Leptonica libraries per OS,
  verifies every artifact against its SHA-256 before embedding, and the
  Release workflow packages portable per-OS archives with
  THIRD_PARTY_NOTICES, an offline README, and a global SHA256SUMS —
  see `docs/packaging.md`.
- Glyphless overlay font (`pdf.ttf`) moved into the verified asset
  bundle; Leptonica bundled and loaded before Tesseract.
- Documentation: product requirements, planned architecture, verification
  plan (honest pending/passed status), third-party notices, security policy.
