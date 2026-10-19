//! Build-time asset-bundle selection (issue #19) with a fail-closed
//! placeholder guard (issue #42).
//!
//! chilli-pdf embeds its asset bundle into the binary at compile time:
//!
//! * **fixture mode (default)** — the committed `assets/v1/` fixture
//!   bundle. Development needs no network: the tiny placeholder blobs
//!   keep every build and test working offline.
//! * **real-assets mode** — when `scripts/fetch-assets` has downloaded
//!   the pinned real libraries and models into the gitignored
//!   `assets/real/` overlay, the crate embeds **those** instead.
//!
//! This script emits `--cfg chilli_real_assets` in real-assets mode so
//! `src/assets.rs` can switch its `include_dir!`/`include_str!` targets.
//!
//! `cargo:rerun-if-changed=assets` makes cargo watch the whole `assets/`
//! tree — **including** the gitignored `assets/real/` overlay. This is
//! required: the default "rerun when any package file changes" behavior
//! respects `.gitignore`, so creating or removing the overlay would
//! otherwise never re-run this script and the mode would silently stay
//! as it was on the previous build. Explicit rerun-if paths bypass the
//! ignore filtering, so `scripts/fetch-assets` (and `rm -rf assets/real`)
//! flip the mode on the very next build.
//!
//! # Fail-closed placeholder guard (issue #42)
//!
//! v1.0.0 shipped a release build whose Tesseract/Leptonica slots held
//! the placeholder fixture blobs: checksum verification passed because
//! the placeholder checksums were pinned in the manifest. This script
//! now refuses to compile such a binary. The canonical placeholder
//! definition is the committed fixture manifest (`assets/manifest.txt`):
//! the six Tesseract/Leptonica entries there ARE the placeholder bytes
//! by definition, so the guard stays correct automatically when the
//! fixtures are regenerated (`examples/update_asset_manifest`).
//!
//! Guard semantics (no bypass, no escape-hatch env var):
//!
//! * **fixture mode + non-debug profile** — `cargo build --release` (or
//!   any custom profile) with only fixture assets present is a
//!   hard error. Release binaries must carry real libraries.
//! * **fixture mode + debug profile** — allowed as a clearly-labeled
//!   DEGRADED build with a loud `cargo:warning`. `cargo test`, `cargo
//!   clippy` and dev builds depend on this offline path.
//! * **real mode, any profile** — every Tesseract/Leptonica file
//!   present in the overlay is hashed; any byte-for-byte match with a
//!   fixture placeholder checksum is a hard error. This catches the
//!   historical failure (placeholder bytes copied into the overlay) no
//!   matter how the manifest was pinned.
//!
//! The script never touches the network; the fetching itself lives in
//! `scripts/fetch-assets` (see `docs/packaging.md`). It uses the same
//! `sha2` crate as the runtime verifier (a build-dependency; the lock
//! file is unchanged because it does not record dependency kinds).

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The native-library slots guarded here. PDFium, the traineddata models
/// and `pdf.ttf` are NOT placeholders in the fixture bundle (they are
/// real, pinned downloads), so only these six OCR engine slots need
/// guarding.
const OCR_LIBRARY_SLOTS: [&str; 6] = [
    "leptonica.dll",
    "libleptonica.dylib",
    "libleptonica.so",
    "tesseract.dll",
    "libtesseract.dylib",
    "libtesseract.so",
];

/// The committed fixture manifest: the canonical placeholder checksums.
const FIXTURE_MANIFEST: &str = "assets/manifest.txt";

fn main() {
    // Declaring the cfg silences the `unexpected_cfgs` lint in both modes.
    println!("cargo:rustc-check-cfg=cfg(chilli_real_assets)");

    // Watch the whole assets tree so that creating or removing the
    // gitignored assets/real/ overlay re-runs this script. See the module
    // docs above for why the default package-change tracking is not enough.
    println!("cargo:rerun-if-changed=assets");

    let placeholders = placeholder_ocr_checksums();

    let real_bundle = Path::new("assets/real/v1");
    let real_manifest = Path::new("assets/real/manifest.txt");
    if real_bundle.is_dir() && real_manifest.is_file() {
        println!("cargo:rustc-cfg=chilli_real_assets");
        println!(
            "cargo:warning=chilli-pdf: embedding REAL bundled assets from assets/real/v1 \
             (run `rm -rf assets/real` to return to the fixture bundle)"
        );
        guard_real_bundle(real_bundle, &placeholders);
    } else {
        guard_fixture_profile();
    }
}

/// Parse the committed fixture manifest and return `<slot> -> sha256` for
/// the six OCR engine library slots. A missing or malformed entry is a
/// hard error: the guard refuses to run on incomplete data rather than
/// silently skipping the check (fail-closed).
fn placeholder_ocr_checksums() -> BTreeMap<&'static str, String> {
    let manifest = std::fs::read_to_string(FIXTURE_MANIFEST).unwrap_or_else(|err| {
        panic!(
            "chilli-pdf placeholder guard: cannot read {FIXTURE_MANIFEST} ({err}); \
             this file defines the placeholder checksums and must be committed"
        )
    });

    let mut checksums = BTreeMap::new();
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        // Format: <sha256-hex> <size-bytes> <relative-path>
        let mut fields = line.split_whitespace();
        let (Some(hex), Some(_size), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!(
                "chilli-pdf placeholder guard: malformed manifest line in {FIXTURE_MANIFEST}: \
                 {line:?}"
            );
        };
        if let Some(slot) = OCR_LIBRARY_SLOTS.iter().find(|slot| path == **slot) {
            if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                panic!(
                    "chilli-pdf placeholder guard: slot {slot} in {FIXTURE_MANIFEST} has a \
                     malformed sha256: {hex:?}"
                );
            }
            checksums.insert(*slot, hex.to_ascii_lowercase());
        }
    }

    for slot in OCR_LIBRARY_SLOTS {
        if !checksums.contains_key(slot) {
            panic!(
                "chilli-pdf placeholder guard: {FIXTURE_MANIFEST} has no entry for OCR library \
                 slot {slot}; regenerate it with \
                 `cargo run --example update_asset_manifest`"
            );
        }
    }
    checksums
}

/// Fixture mode: every embedded Tesseract/Leptonica file IS a placeholder
/// by definition, so the guard reduces to a profile check. Debug builds
/// (dev, `cargo test`, `cargo clippy`) proceed with a DEGRADED warning;
/// everything else — notably `cargo build --release` — is refused.
fn guard_fixture_profile() {
    let profile = std::env::var("PROFILE").unwrap_or_else(|err| {
        panic!(
            "chilli-pdf placeholder guard: cargo did not set PROFILE ({err}); refusing to \
             build without knowing the profile (fail-closed)"
        )
    });
    if profile == "debug" {
        println!(
            "cargo:warning=chilli-pdf: DEGRADED build — embedding the fixture bundle with \
             PLACEHOLDER Tesseract/Leptonica blobs. Tests and examples that exercise real \
             engines must refuse to run against this binary. Run scripts/fetch-assets and \
             rebuild for a real bundle; see docs/packaging.md."
        );
    } else {
        panic!(
            "chilli-pdf placeholder guard: refusing to build profile {profile:?} against the \
             fixture asset bundle — the Tesseract/Leptonica slots would contain placeholder \
             bytes (issue #42). Either build the debug profile (tests only) or fetch the real \
             assets first: `scripts/fetch-assets` then rebuild. See docs/packaging.md."
        );
    }
}

/// Real mode: hash every guarded OCR library file present in the overlay
/// and refuse the build if any of them is byte-identical to the committed
/// fixture placeholder. Runs in every profile — the overlay only exists
/// to hold real libraries, so there is no legitimate placeholder here.
fn guard_real_bundle(real_bundle: &Path, placeholders: &BTreeMap<&'static str, String>) {
    for (slot, placeholder) in placeholders {
        let Some(bytes) = read_slot(real_bundle, slot) else {
            continue;
        };
        let digest = Sha256::digest(&bytes);
        let hex_digest = hex::encode(digest);
        if hex_digest == *placeholder {
            panic!(
                "chilli-pdf placeholder guard: assets/real/v1/{slot} is byte-identical to the \
                 committed fixture placeholder (sha256 {placeholder}) — issue #42 would \
                 repeat. The real-assets overlay must contain the real library. Re-run \
                 scripts/fetch-assets with real pins, or remove the overlay with \
                 `rm -rf assets/real` to return to fixture/debug builds. \
                 See docs/packaging.md."
            );
        }
    }
}

/// Read one guarded slot from the overlay; `None` means the platform does
/// not place a file with that name (each platform only carries its own
/// tesseract/leptonica pair).
fn read_slot(real_bundle: &Path, slot: &str) -> Option<Vec<u8>> {
    let path = real_bundle.join(slot);
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!(
            "chilli-pdf placeholder guard: cannot read {} ({err}); refusing to build with \
             an unreadable OCR library slot (fail-closed)",
            path.display()
        ),
    }
}
