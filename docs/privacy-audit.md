# Privacy audit: no runtime network, no document contents in errors/logs

Issue #20. This document defines the two privacy guarantees from
`docs/product-requirements.md` ("Privacy & network" and "Errors must be
useful without logging document contents") as checkable rules, records the
repeatable audit method for both, and reports the results for the current
tree. Re-run the whole audit for every release; record the outcome in
`docs/verification-plan.md`.

## The allowed message set (definition)

"Document contents" means any byte, word, line, glyph, or value derived
from the input document: page text (extracted or OCR-recognized), image
pixels, document metadata (author, title, keywords), strings embedded in
the file (file names, annotations, JavaScript), PDF dictionary keys or
values, and raw buffer excerpts.

An error message, log line, status text, or progress event may contain
ONLY:

| Category          | Examples                                                      |
|-------------------|---------------------------------------------------------------|
| Phase/step names  | "Converting…", "Adding the searchable text layer"             |
| Page numbers      | 1-based page indices and page counts                          |
| Error classes     | Fixed, pre-written descriptions ("password-protected", "malformed", "could not be rendered") |
| Paths             | Input/output file paths as chosen by the user                  |
| Sizes             | Byte sizes (size caps), image dimensions in pixels            |
| Language codes    | `eng`, `hun` (OCR preset codes, not document data)             |
| Component names   | Asset file names, library symbols, asset SHA-256 digests       |
| OS/IO error text  | `io::Error` strings (paths, errno descriptions) — cannot carry document bytes because they are produced by the OS about files, not about buffers |
| Fixed UI guidance | "Pick a different name or folder", "try again" style help     |

Everything else — in particular anything from the table's forbidden
counterpart (document text, pixels, metadata) — is a content leak and a
bug. Paths are allowed because the user chose them; the user's own file
name is not treated as document content.

## Part 1 — dependency network gate (CI-enforced)

### What is enforced

`deny.toml` at the repo root configures `cargo deny`. The CI job `deny`
in `.github/workflows/ci.yml` runs:

```
cargo deny check bans licenses
```

- **bans** — a deny list of network-capable crates: HTTP clients and
  protocol machinery (`reqwest`, `ureq`, `curl`, `hyper`, `h2`, …), TLS
  stacks (`native-tls`, `openssl`, `rustls`, …), DNS resolvers
  (`hickory-*`, `trust-dns-*`, `c-ares`), WebSockets (`tungstenite`
  family), remote-access protocols (`ssh2`, ftp, SMTP mailers, network
  database clients), and the low-level async-net drivers `mio`,
  `socket2`, `net2`. Adding any of these to the tree fails CI.
- **licenses** — every crate must be under one of the permissive licenses
  in the allow list (MIT, Apache-2.0, ISC, BSD, Zlib, MPL-2.0, BSL-1.0,
  Unicode-3.0, OFL-1.1/Ubuntu-font for the bundled egui fonts).
- **advisories are deliberately NOT checked in CI**: the advisory
  database is a git repository fetched over the network, which would
  make the gate network-bound and flaky. `bans` and `licenses` need no
  advisory DB. A local, network-optional advisories pass is part of the
  manual audit below.

### The tokio exception (feature-combo, not crate)

`tokio` IS in the tree — pulled only as a wasm-target dependency of
`wasm-bindgen-futures` (via pdfium-render/rfd/wgpu), with features
limited to `rt` and `time`. Banning tokio outright would break the GUI
stack without buying anything: tokio's `net` feature cannot link without
`mio` and `socket2`, both of which are banned. So the gate blocks the
*feature-combo* `tokio/net` structurally, and the CI job additionally
fails directly if `cargo tree` ever shows the `net` feature enabled:

```
cargo tree --target all -e features | grep 'tokio feature "net"'
```

(expected to produce no output; the CI step exits non-zero on a match).

### Reviewed-and-accepted crates (not banned, with reasons)

- `webbrowser` (via `egui-winit`, with its `url` dependency) — can hand
  a URL to the OS browser. It performs no network I/O itself and is
  only invoked if application code calls it; no chilli-pdf code path
  does. Watch: this crate must never be called from our code.
- `rustix`, `polling`, `async-io`, `zbus` (Linux accessibility/ATSPI
  stack) — general syscall/event-loop layers required by the windowing
  stack; they CAN create sockets in principle, but are only exercised
  over D-Bus/wayland/local IPC. The runtime firewall test (see
  "Pending") is the enforcement point here.
- `url`, `idna`, `form_urlencoded` — pure parsers, no I/O.

### Vetting a new dependency (checklist)

Before adding any dependency (direct or transitive) to `Cargo.toml`:

1. Does the crate (or any feature of it) open sockets, resolve DNS,
   speak HTTP/TLS/SSH/SMTP, or spawn network-capable processes? If yes:
   STOP — the offline requirement must change first, not the gate.
2. Run `cargo deny check bans licenses` — must pass.
3. Run `cargo tree --target all -e features` and confirm no new
   feature-combo enables `tokio/net` or any other net-capable feature.
4. Does the crate's error type ever embed input data (parsed content)
   in its `Display`/`Debug` output? If it does, do not let its `to_string()`
   reach any user-facing or logged message; map it to a typed error at
   the boundary the way `src/overlay/merge.rs` maps `lopdf` errors.
5. Record the addition (crate, version, purpose, checklist answers)
   in this file's results table when re-running the audit.

### Manual full audit (local, network allowed)

```
cargo deny --version                 # expect: cargo-deny 0.20.2 (pinned in CI)
cargo deny check bans licenses       # the exact CI gate — must pass
cargo deny check advisories          # optional, needs the advisory DB
cargo tree --target all -e features | grep 'tokio feature "net"'   # expect no output
```

## Part 2 — content-leak audit (code)

### Repeatable grep method

Run these from the repo root; all of `src/` is in scope (including
`#[cfg(test)]` code, which may embed fixtures but never document
content by the same rule — test fixtures are committed files, not
user documents):

```
# Every message/format/print/log construction site:
grep -rn "format!\|eprintln!\|println!\|print!\|panic!\|write!" src/ --include="*.rs"

# The user-facing message functions:
grep -rn "fn user_message\|Display for" src/ --include="*.rs"

# Network surface in our own code (expect no output):
grep -rn "std::net\|TcpStream\|UdpSocket\|SocketAddr\|lookup_host" src/ --include="*.rs"

# Logging frameworks (expect no output; use word boundaries to avoid
# false positives like FileDialog):
grep -rnE "\bdbg!|\blog::|\btracing::|\benv_logger" src/ --include="*.rs"
```

Then review EVERY match against the allowed message set above. The
audit is complete only when every match is classified.

### Results for the current tree (2026-10-18, commit range up to 2208f03)

| Site                                   | What it may embed                                    | Verdict |
|----------------------------------------|-------------------------------------------------------|---------|
| `src/pdf/error.rs` (PdfError)          | fixed text, paths, page numbers, page counts            | clean; pinned by `error_messages_never_leak_document_contents` |
| `src/ocr/error.rs` (OcrError)          | fixed text, paths, language codes, image dimensions, OS/library-load messages | clean; pinned by `error_messages_are_content_free` (added by this audit) |
| `src/overlay/error.rs` (OverlayError)  | fixed text, paths, page numbers, counts                | clean; pinned by `error_messages_never_leak_document_contents` |
| `src/validation.rs` (ValidationError)  | fixed text, byte sizes                                 | clean; pinned by `error_messages_never_leak_document_contents` (drives a SECRET-containing fixture through the full path) |
| `src/transactional.rs` (TransactionError, PublishError) | fixed text (user_message), paths + `io::Error` (Display) | clean |
| `src/worker.rs` (ConversionError, worker OverlayError) | delegates to the message functions above; `OverlayError::Failed` only wraps `src/overlay`'s typed errors or `io::Error` | clean |
| `src/converter.rs` (WorkerEvent::Failed) | carries `ConversionError::user_message()` strings only  | clean |
| `src/app_state.rs` (Status::Error/Info) | fixed text, paths, `WorkerEvent::Failed` payloads      | clean |
| `src/main.rs` (println/eprintln)       | version string; eframe window-open error (GUI framework, not document data) | clean |
| `src/assets.rs` (manifest panic)       | parse error of the compile-time-embedded manifest (asset names/digests, never document data) | clean |
| `src/pdf/render.rs`                    | maps pdfium-render errors to typed `PdfError` (drops the source message) | clean |
| `src/ocr/ffi.rs`, `src/ocr/image.rs`   | libloading/OS messages (paths), dimension-only `InvalidImage` messages | clean |
| `src/overlay/merge.rs`                 | every `lopdf` error mapped with `map_err(\|_\| …)` to a typed content-free variant — lopdf `Display` strings never escape | clean |

`panic!` sites outside `#[cfg(test)]`: only `src/assets.rs`'s embedded
manifest invariant (compile-time asset data, not user input). All other
`panic!`/`unwrap`/`expect` hits are inside test modules. The grep "own
network surface" produces no output. The logging-framework grep produces
no output (the raw `\blog::` pattern can false-positive on
`FileDialog`-style identifiers; use the word-boundary form above).

Unit tests pinning the format functions (the "inspect the formatting
functions" check, per failure class):

- bad PDF: `validation::tests::error_messages_never_leak_document_contents`,
  `pdf::error::tests::error_messages_never_leak_document_contents`
- OCR failure: `ocr::error::tests::error_messages_are_content_free`
  (added by this audit; test-only change, no production code touched)
- overlay failure: `overlay::error::tests::error_messages_never_leak_document_contents`
- pipeline/GUI chain: `WorkerEvent::Failed` only ever receives
  `user_message()` output; `app_state` tests assert the exact status
  texts.

### Gate negative controls (2026-10-18, cargo-deny 0.20.2)

Adding `socket2 = "0.6"` to a scratch crate with this `deny.toml`:
`bans FAILED` (banned). Adding `tokio = { features = ["net"] }`:
`bans FAILED` (`mio` and `socket2` both flagged as banned children of
tokio). The current tree: `bans ok, licenses ok` (only benign
duplicate-version warnings, configured as warnings).

## Pending (cannot be verified on this machine)

The strongest form of the network promise — zero connection attempts at
runtime, observed from outside the process (firewall rules or a
network-isolated VM) — belongs to the acceptance phase (issue #21) on a
real machine and stays ⏳ pending in `docs/verification-plan.md`. The
CI dependency gate and the static audit above are the enforceable parts
available now; they prove no network-capable code is linked and no
message path can carry document contents, but they cannot prove the
absence of OS-level connections. Never mark the runtime check as passed
without running it.
