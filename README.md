# chilli-pdf

**Make scanned PDFs searchable — fully offline, on Windows, macOS, and Linux.**

chilli-pdf takes a scanned PDF (pictures of pages, no selectable text) and
produces a copy where the text is selectable, searchable, and copyable. The
original page appearance, dimensions, and rotation are preserved: recognized
text is added as an *invisible* layer aligned on top of the original page.

Everything runs on your computer. No accounts, no telemetry, no update
checks, no runtime downloads, no separately installed OCR engine.

## Features

- **One-click conversion.** Pick a scanned PDF, choose the OCR language
  (English, Hungarian, or English + Hungarian), press **Convert**. Progress
  is shown per page and **Cancel** works at any time.
- **Searchable, copyable text.** The recognized text is placed as an
  invisible layer aligned with the words on the page, so search hits and
  selections land on the right spot — including rotated and cropped pages,
  and Hungarian `ő`/`ű`.
- **The original is preserved.** Pages are never re-rasterized; appearance,
  page count, dimensions, rotation, bookmarks, links, and metadata are
  carried over untouched, and the input file is never modified.
- **Skips pages that already have text.** In a mixed document the
  already-searchable pages are left completely alone; only scanned pages
  get the text layer.
- **Safe by default.** Output goes to `<original_name>_searchable.pdf`,
  is written transactionally (temp file + atomic rename, no partial
  output), and existing files are only replaced after an explicit prompt.
- **Clear refusals instead of silent damage.** Password-protected,
  digitally signed, and malformed PDFs are rejected up front with a clear
  explanation. Nothing is half-converted.
- **Fully offline, single executable.** The PDF engine (PDFium), the OCR
  engine (Tesseract), and the language models are bundled inside the
  binary and extracted to a versioned per-user directory with checksum
  verification on first launch. Nothing to install, no admin rights.

## Platforms

| OS | Archive | Notes |
|----|---------|-------|
| Windows 10/11 x64 | `chilli-pdf-windows-x64.zip` | no console window flashes in release builds |
| macOS (Apple Silicon) | `chilli-pdf-macos.tar.gz` | Intel Macs are out of scope for v1 |
| Linux x64 | `chilli-pdf-linux-x64.tar.gz` | needs a standard desktop with GTK 3 (file dialogs); X11 and Wayland both supported by the windowing stack |

## Install

Download the portable archive for your OS from the
[Releases](https://github.com/aron98/chilli-pdf/releases) page, unpack it,
and run the executable — there is no installer and nothing else to set up.
Each archive contains the binary, `LICENSE`, `THIRD_PARTY_NOTICES.md`, and
a short `README.txt`.

On first launch chilli-pdf prepares its bundled components (PDF engine, OCR
engine, language models) in your personal application-data directory. That
happens once per version, needs no administrator rights, and works with the
network disabled.

Verify your download against the `SHA256SUMS` file published next to the
archives (`sha256sum` on Linux/macOS, `Get-FileHash` on Windows).

**macOS note:** the binary is unsigned, so Gatekeeper will ask for
confirmation on first launch — right-click → **Open** (or System Settings →
Privacy & Security → **Open Anyway**) to proceed.

## Build from source

Requires the Rust toolchain ([rustup](https://rustup.rs/) recommended); the
repo pins the exact version via `rust-toolchain.toml`. On Linux, install the
GTK 3 development packages first (`sudo apt-get install libgtk-3-dev` on
Ubuntu/Debian).

```sh
git clone https://github.com/aron98/chilli-pdf.git
cd chilli-pdf
cargo build --release   # binary at target/release/chilli-pdf[-.exe]
cargo test              # 224 tests, offline
```

The default build embeds the committed fixture bundle (tiny placeholder
engine libraries) so it needs no network — but it cannot run real OCR. To
build with the real, pinned engines:

```sh
scripts/fetch-assets          # downloads + SHA-256-verifies the pinned
                              # PDFium / Tesseract / Leptonica / tessdata
cargo build --release         # embeds assets/real/ instead of the fixtures
```

The fetch step is build-time only; the finished app never touches the
network. See [docs/packaging.md](docs/packaging.md) for the full packaging
story.

## Limitations

- One document at a time — no batch mode in v1.
- Partially searchable pages are out of scope: a page either already
  carries meaningful text (skipped) or is scanned (gets the OCR layer).
- Password-protected (encrypted) and digitally signed PDFs are refused in
  v1; use an unprotected, unsigned copy.
- OCR quality is bounded by Tesseract and the bundled `tessdata_fast`
  models; handwriting recognition and table reconstruction are out of
  scope.

See [docs/preservation.md](docs/preservation.md) for exactly what is
preserved during conversion and the remaining edge cases.

## Privacy & offline guarantee

- **No network at runtime — ever.** All processing is local. Build-time
  downloads (asset fetch) never run on your machine; the shipped binary
  makes zero network requests.
- **No document contents in errors or logs.** Error messages carry phases,
  page numbers, paths, and sizes — never extracted text, images, or
  metadata from your documents.
- The dependency tree is CI-enforced network-free (`cargo deny` bans any
  network-capable crate) and the message surface is audited — method and
  results in [docs/privacy-audit.md](docs/privacy-audit.md).

## Verification status

The acceptance checks are tracked honestly, check by check, in
[docs/verification-report.md](docs/verification-report.md): 17/17 local
driver checks passed with the real engines (including a zero-network-syscall
run under `strace`), while the checks that require a clean offline Windows
VM, real PDF viewers, or the release binary are explicitly listed as
pending — never claimed as done.

## CLI

```
chilli-pdf [INPUT.pdf]   # opens the native window, INPUT pre-selected
chilli-pdf --version
chilli-pdf --help
```

## License

- Application code: MIT (see [LICENSE](LICENSE)).
- Bundled components: PDFium, Tesseract, Leptonica, and OCR traineddata
  under their respective licenses — see
  [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Reporting security issues

See [SECURITY.md](SECURITY.md). Please report suspected vulnerabilities
privately rather than opening a public issue.
