//! Regression tests for the build.rs placeholder guard (issue #42).
//!
//! The guard's data source is the committed fixture manifest
//! (`assets/manifest.txt`): the six Tesseract/Leptonica entries there are
//! the canonical placeholder checksums. If that file loses an entry (or
//! the fixture blobs are regenerated without re-running
//! `examples/update_asset_manifest`), the guard would either panic on
//! every build or — worse — silently stop recognizing placeholders. These
//! tests pin the contract so the guard stays functional in both bundle
//! modes; they read the fixture manifest directly and run in any mode.

/// The OCR engine library slots guarded by build.rs. Kept in sync with
/// `OCR_LIBRARY_SLOTS` there; pdfium/traineddata/pdf.ttf are real assets
/// in the fixture bundle and are intentionally not listed.
const OCR_LIBRARY_SLOTS: [&str; 6] = [
    "leptonica.dll",
    "libleptonica.dylib",
    "libleptonica.so",
    "tesseract.dll",
    "libtesseract.dylib",
    "libtesseract.so",
];

/// Manifest lines look like `<sha256-hex> <size-bytes> <relative-path>`
/// with `#` comments. Mirrors the parsing in build.rs.
fn manifest_entries() -> Vec<(String, u64, String)> {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/manifest.txt"))
            .expect("committed fixture manifest assets/manifest.txt must exist");
    let mut entries = Vec::new();
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (hex, size, path) = (
            fields.next().expect("manifest line with sha256"),
            fields.next().expect("manifest line with size"),
            fields.next().expect("manifest line with path"),
        );
        let size: u64 = size
            .parse()
            .unwrap_or_else(|_| panic!("manifest size for {path:?} is not a number: {size:?}"));
        entries.push((hex.to_string(), size, path.to_string()));
    }
    entries
}

#[test]
fn fixture_manifest_lists_every_ocr_library_slot_with_valid_checksum() {
    let entries = manifest_entries();
    for slot in OCR_LIBRARY_SLOTS {
        let Some((hex, _size, _path)) = entries
            .iter()
            .find(|(_hex, _size, path)| path == slot)
            .map(|(hex, size, path)| (hex.as_str(), *size, path.as_str()))
        else {
            panic!("assets/manifest.txt has no entry for OCR library slot {slot}");
        };
        assert_eq!(
            hex.len(),
            64,
            "slot {slot} must pin a full sha256, got {hex:?}"
        );
        assert!(
            hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "slot {slot} has a malformed sha256: {hex:?}"
        );
    }
}

#[test]
fn fixture_ocr_library_blobs_are_small_placeholders() {
    // The fixture OCR libraries are deliberately tiny placeholders (~1-2 KB);
    // a real Tesseract/Leptonica is orders of magnitude larger. This bounds
    // the guard's definition of "placeholder bytes": if a slot ever grows to
    // realistic size, the fixtures have been replaced by real libraries and
    // the committed manifest (and the guard's checksums) must be regenerated.
    const PLACEHOLDER_SIZE_LIMIT: u64 = 16 * 1024;
    let entries = manifest_entries();
    for slot in OCR_LIBRARY_SLOTS {
        let (_hex, size, _path) = entries
            .iter()
            .find(|(_hex, _size, path)| path == slot)
            .unwrap_or_else(|| panic!("assets/manifest.txt has no entry for slot {slot}"));
        assert!(
            *size <= PLACEHOLDER_SIZE_LIMIT,
            "fixture OCR library {slot} is {size} bytes — larger than the placeholder \
             limit ({PLACEHOLDER_SIZE_LIMIT}); regenerate assets/manifest.txt with \
             `cargo run --example update_asset_manifest`"
        );
    }
}
