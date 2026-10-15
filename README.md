# chilli-pdf

**Make scanned PDFs searchable — fully offline, on Windows, macOS, and Linux.**

chilli-pdf takes a scanned PDF (pictures of pages, no selectable text) and
produces a copy where the text is selectable, searchable, and copyable. The
original page appearance, dimensions, and rotation are preserved: recognized
text is added as an *invisible* layer aligned on top of the original page.

Everything runs on your computer. No accounts, no telemetry, no update
checks, no runtime downloads, no separately installed OCR engine.

## Status

**Scaffolding stage.** The repository currently contains the project
foundation — buildable, tested source skeleton, CI, docs, and the roadmap —
but **the conversion pipeline and native GUI are not implemented yet**. They
are being built issue by issue against the milestones below. Nothing has been
verified on a clean machine yet; see `docs/verification-plan.md` for the
honest, always-current status of every acceptance check.

| Milestone | Scope |
|-----------|-------|
| v0.1.0 | End-to-end MVP: native GUI + PDFium rendering + Tesseract OCR (eng/hun/eng+hun) + invisible text overlay, Windows first |
| v0.2.0 | Reliability & packaging: transactional output, bundled assets, signed release artifacts for all three platforms |
| v1.0.0 | Cross-platform acceptance testing, docs, and the first fully verified release |

## Design principles

- **Offline only.** Build-time downloads are fine; runtime network access is
  not. All OCR models (English, Hungarian) ship inside the application.
- **One portable binary.** All native libraries and traineddata files are
  bundled; on first launch they are extracted to a versioned per-user
  directory with checksum verification. Nothing to install, no admin
  rights.
- **Never damage the input.** Output goes to `<original_name>_searchable.pdf`
  by default, is written transactionally, and existing files are only
  replaced after an explicit prompt.
- **Preserve the original.** Pages are never re-rasterized; recognized text
  is overlaid invisibly so appearance, rotation, bookmarks, and links stay
  intact.

See [docs/product-requirements.md](docs/product-requirements.md) for the
full requirements and [docs/architecture.md](docs/architecture.md) for the
planned technical design.

## Building

Requires only the Rust toolchain (the repo pins it via `rust-toolchain.toml`):

```sh
git clone https://github.com/aron98/chilli-pdf.git
cd chilli-pdf
cargo build --release   # binary at target/release/chilli-pdf[-.exe]
cargo test              # unit tests
cargo fmt --check && cargo clippy -- -D warnings
```

CI builds and tests every commit on `ubuntu-latest`, `macos-latest`, and
`windows-latest`; releases are produced by the `Release` workflow on `v*`
tags. Release packaging (bundling PDFium/Tesseract and OCR models) is
milestone work — see the tracker.

## CLI

```
chilli-pdf [INPUT.pdf]   # opens the native window once the GUI lands
chilli-pdf --version
chilli-pdf --help
```

## License

- Application code: MIT (see [LICENSE](LICENSE)).
- Bundled components: PDFium, Tesseract, Leptonica, and OCR traineddata
  under their respective licenses — see
  [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) (finalized at first
  release).

## Reporting security issues

See [SECURITY.md](SECURITY.md). Please report suspected vulnerabilities
privately rather than opening a public issue.
