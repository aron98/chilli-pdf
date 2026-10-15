# Verification plan

Single source of truth for what has — and has not — been verified. This
file is updated with every milestone. **No check is marked "passed" without
being actually executed**, and limitations are stated explicitly.

## Current status: scaffold only

As of this writing the repository contains a buildable, tested skeleton
(compiles and passes `cargo test` on the dev machine) but the conversion
pipeline and GUI are not implemented. **None of the acceptance checks below
have been run yet.** Windows execution is not available in the current
development environment; any Windows-specific verification will be
performed on a clean Windows 10/11 VM and reported here, never assumed.

## Acceptance checks (from product requirements)

| # | Check | Status |
|---|-------|--------|
| 1 | Run final executable on a clean Windows machine/VM, no dev tools, no separate dependencies, networking disabled from first launch | ⏳ pending |
| 2 | Convert an English scan, a Hungarian scan, and a mixed-language document | ⏳ pending |
| 3 | Verify actual text extraction and representative search/copy results incl. Hungarian accents (`ő`, `ű`) in Edge **and** at least one other PDF viewer | ⏳ pending |
| 4 | Check alignment, appearance, page count, page dimensions, and rotation against the input | ⏳ pending |
| 5 | Test mixed searchable/scanned pages, cancellation, malformed input, password-protected input, Unicode paths, output collisions, and a larger multipage document | ⏳ pending |
| 6 | Confirm runtime processing makes no network requests | ⏳ pending |

## Standing per-release checks

- [ ] `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings` green on all three CI platforms
- [ ] Release artifacts built by CI with SHA-256 checksums published
- [ ] Bundled asset manifest verified against embedded checksums at extraction
- [ ] No document contents in any error message or log
- [ ] Temporary files removed after success, cancellation, and failure
- [ ] Input file byte-identical before and after a conversion (checksum)
- [ ] Output file absent after a cancelled or failed conversion
- [ ] `THIRD_PARTY_NOTICES.md` complete and shipped with the binary

## Environment limitations to disclose in the release report

- Where Windows/macOS/Linux execution was unavailable (e.g. no Windows VM),
  the report must say so explicitly and list which checks remain pending.
