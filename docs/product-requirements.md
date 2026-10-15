# Product requirements (v1)

Source of truth for what "done" means for chilli-pdf version one. Every
acceptance item is mirrored in `docs/verification-plan.md`, where actual
verification status is tracked honestly.

## Purpose

A minimal, native desktop application that converts **scanned PDFs into
searchable PDFs** with selectable, copyable text — completely offline,
including first launch on a clean computer.

## Platforms

- Version one targets **Windows 10/11 x64** first; the product is
  cross-platform by design and must also build and run on **macOS** and
  **Linux**.
- Small native desktop interface; **no browser or WebView runtime**.

## User-facing workflow

1. Pick one input PDF (one file at a time).
2. Choose the OCR language: English, Hungarian, or English + Hungarian.
3. Press **Convert**; a progress indicator shows per-page progress.
4. **Cancel** is available at any time.
5. Choose the output location; default is `<original_name>_searchable.pdf`.

## OCR

- English, Hungarian, and combined English + Hungarian recognition.
- **All language models are bundled**; nothing is downloaded at runtime.
- ~300 DPI render for recognition, with safeguards for unusually large
  pages.
- Ordinary printed documents are in scope. Handwriting and complex table
  reconstruction are **out of scope** for v1.

## PDF behavior

- Preserve original page appearance, dimensions, rotation, page order, and
  page content — render images for recognition but **never replace original
  page content with newly rasterized images**.
- Add correctly positioned **invisible text** (search/selection/copy work;
  searching highlights the right area).
- Handle Unicode correctly, including Hungarian `ő` and `ű`.
- Skip pages that already contain meaningful extractable text. A single
  stray character must not classify a page as searchable. Partially
  searchable pages are outside initial scope; the heuristic and its
  limitation are documented.
- Preserve bookmarks, links, and metadata **where supported**; verify
  preservation and disclose remaining limitations.
- **Reject** password-protected PDFs with a clear message.
- **Reject** digitally signed PDFs with a clear explanation (modification
  would invalidate signatures).
- **Fail clearly** on malformed/unsupported documents. Never silently
  return a partially converted PDF as a successful result.

## Privacy & network

- All processing stays on the local computer.
- No accounts, telemetry, update checks, network requests, or runtime
  downloads — ever.
- Errors must be useful **without logging document contents**.

## Distribution

- Exactly **one user-facing executable** per platform; all libraries, OCR
  models, and notices bundled inside it.
- Extraction of bundled components into a per-user application directory is
  acceptable; the user must not install or configure anything.
- No administrator rights, no Python, no separately installed OCR engine,
  no package managers, no separately installed runtimes.
- Normal OS components are acceptable dependencies.
- Build-time downloads acceptable; **runtime network access is not**.
- Extraction safety: versioned directories, verification of bundled assets,
  and no loading of libraries from the working directory or other
  uncontrolled locations.

## Reliability

- UI stays responsive during conversion.
- Bounded memory usage: process page by page; never hold every rendered page
  in memory.
- Respect page rotation and crop boxes when positioning text.
- Handle filenames with spaces and Unicode.
- **Transactional output:** publish the final file only after successful
  conversion. Cancel/failure leaves the input intact and no misleading
  output.
- Remove temporary files after success, cancellation, or failure.
- **Never overwrite the input.** Ask before replacing an existing output.

## Out of scope for v1

PDF editing, cloud integration, batch processing, handwriting recognition,
table reconstruction, and any unrelated features.
