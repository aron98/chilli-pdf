//! Deterministic synthetic fixture generator for the integration test
//! harness (issue #18).
//!
//! Every `gen_*.pdf` fixture under `tests/fixtures/` is produced by this
//! file and committed to the repository, so plain `cargo test` — on any OS,
//! with no Python, Tesseract or other system dependencies — can run the
//! pipeline integration tests in `tests/integration.rs` against real PDF
//! bytes.
//!
//! The fixtures are hand-assembled PDFs: each one is built from literal
//! objects (catalog, page tree, pages, content streams and one small image),
//! so the bytes are fully deterministic — no timestamps, no randomness —
//! and regeneration is byte-for-byte reproducible. All image fixtures wrap
//! a small DeviceGray pattern image in a page-filling `/Im0 Do` draw, which
//! is what a scanner pipeline produces ("scanned-style": image-wrapped
//! pages, no text layer). The English and Hungarian fixtures differ only
//! in their pixel pattern; the Hungarian *words* live in the scripted OCR
//! used by `tests/integration.rs`, because a hand-written PDF generator
//! cannot rasterise glyphs without pulling in a font rasteriser.
//!
//! To regenerate the committed fixtures after changing a generator:
//!
//! ```text
//! CHILLI_REGEN_FIXTURES=1 cargo test --test fixtures_gen
//! ```
//!
//! By default the `committed_fixtures_match_the_generator` test asserts
//! that the committed files are byte-identical to the generator output,
//! which keeps the generator and the fixtures from drifting apart.

use std::fs;
use std::path::PathBuf;

use lopdf::{Dictionary, Document, Object};

const REGEN_ENV: &str = "CHILLI_REGEN_FIXTURES";

/// Side length (pixels) of the synthetic scan image used by image pages.
const IMAGE_SIDE: usize = 64;

// --- raw PDF assembly ---------------------------------------------------

/// One indirect PDF object: the complete body between `N 0 obj` and
/// `endobj`. Object number `i` is the `i`-th entry of the object list.
type ObjectBody = Vec<u8>;

/// Serialises ordered objects into a complete PDF file, using the same
/// hand-written cross-reference format as `tests/pdf_render.rs`.
fn assemble(objects: &[ObjectBody]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    // A comment line with high-bit bytes signals binary content, as
    // recommended by the PDF specification. Fixed bytes keep the output
    // deterministic.
    pdf.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);

    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// Wraps `content` in an uncompressed stream object.
fn stream_object(content: &[u8]) -> ObjectBody {
    let mut body = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(b"\nendstream");
    body
}

fn catalog_object() -> ObjectBody {
    b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()
}

fn page_tree_object(page_numbers: &[usize]) -> ObjectBody {
    let kids: Vec<String> = page_numbers.iter().map(|n| format!("{n} 0 R")).collect();
    format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        page_numbers.len()
    )
    .into_bytes()
}

/// A page that draws the scan image across the whole media box.
fn image_page_object(
    parent: usize,
    media_box: &str,
    rotate: Option<u32>,
    contents: usize,
    image: usize,
) -> ObjectBody {
    let rotate = rotate
        .filter(|degrees| *degrees != 0)
        .map_or_else(String::new, |degrees| format!(" /Rotate {degrees}"));
    format!(
        "<< /Type /Page /Parent {parent} 0 R /MediaBox [{media_box}]{rotate} \
/Resources << /XObject << /Im0 {image} 0 R >> /ProcSet [/PDF /ImageB] >> \
/Contents {contents} 0 R >>"
    )
    .into_bytes()
}

fn draw_image_content(width: u32, height: u32) -> Vec<u8> {
    format!("q\n{width} 0 0 {height} 0 0 cm\n/Im0 Do\nQ\n").into_bytes()
}

/// A page carrying a genuine (ASCII) text layer, so the pipeline must
/// classify it as AlreadySearchable and leave it untouched.
const HYBRID_TEXT: &str = "Chilli PDF keeps pages that already carry a text layer untouched.";

fn text_page_object(parent: usize, contents: usize, font: usize) -> ObjectBody {
    format!(
        "<< /Type /Page /Parent {parent} 0 R /MediaBox [0 0 612 792] \
/Resources << /Font << /F1 {font} 0 R >> >> /Contents {contents} 0 R >>"
    )
    .into_bytes()
}

fn text_page_content() -> Vec<u8> {
    format!("BT\n/F1 12 Tf\n72 720 Td\n({HYBRID_TEXT}) Tj\nET\n").into_bytes()
}

fn helvetica_font_object() -> ObjectBody {
    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec()
}

fn image_xobject(data: &[u8]) -> ObjectBody {
    let mut body = format!(
        "<< /Type /XObject /Subtype /Image /Width {IMAGE_SIDE} /Height {IMAGE_SIDE} \
/ColorSpace /DeviceGray /BitsPerComponent 8 /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

// --- deterministic scan patterns ----------------------------------------

/// Noisy inked bands: looks like a scanned page of dense text.
fn english_scan_pattern() -> Vec<u8> {
    let mut data = Vec::with_capacity(IMAGE_SIDE * IMAGE_SIDE);
    for y in 0..IMAGE_SIDE {
        for x in 0..IMAGE_SIDE {
            let band = y % 8 < 4;
            let speckle = (x.wrapping_mul(7) ^ y.wrapping_mul(13)).is_multiple_of(5);
            data.push(if band ^ speckle { 0x32 } else { 0xD2 });
        }
    }
    data
}

/// Ragged paragraph lines: looks like a scanned page of running text.
fn hungarian_scan_pattern() -> Vec<u8> {
    let mut data = vec![0xD8; IMAGE_SIDE * IMAGE_SIDE];
    for line in 0..12usize {
        let top = 4 + line * 5;
        let width = 34 + (line * 17) % 24;
        for y in top..(top + 3).min(IMAGE_SIDE) {
            for x in 0..width {
                data[y * IMAGE_SIDE + x] = 0x32;
            }
        }
    }
    data
}

// --- fixture generators --------------------------------------------------

/// One scanned-style letter page: the English fixture.
fn gen_eng_pdf() -> Vec<u8> {
    letter_scan_pdf(&english_scan_pattern())
}

/// One scanned-style letter page: the Hungarian fixture. The Hungarian
/// words themselves are supplied by the scripted OCR in
/// `tests/integration.rs`; the pattern only makes the two fixtures
/// visually distinct.
fn gen_hun_pdf() -> Vec<u8> {
    letter_scan_pdf(&hungarian_scan_pattern())
}

fn letter_scan_pdf(pattern: &[u8]) -> Vec<u8> {
    assemble(&[
        catalog_object(),
        page_tree_object(&[3]),
        image_page_object(2, "0 0 612 792", None, 4, 5),
        stream_object(&draw_image_content(612, 792)),
        image_xobject(pattern),
    ])
}

/// Page one carries a real text layer, page two is a scanned image: the
/// mixed-document case the pipeline must handle by overlaying only the
/// scanned page.
fn gen_hybrid_pdf() -> Vec<u8> {
    assemble(&[
        catalog_object(),
        page_tree_object(&[3, 5]),
        text_page_object(2, 4, 7),
        stream_object(&text_page_content()),
        image_page_object(2, "0 0 612 792", None, 6, 8),
        stream_object(&draw_image_content(612, 792)),
        helvetica_font_object(),
        image_xobject(&english_scan_pattern()),
    ])
}

/// One landscape page tagged `/Rotate 90`: the rotated-scan case.
fn gen_rotated_pdf() -> Vec<u8> {
    assemble(&[
        catalog_object(),
        page_tree_object(&[3]),
        image_page_object(2, "0 0 792 612", Some(90), 4, 5),
        stream_object(&draw_image_content(792, 612)),
        image_xobject(&english_scan_pattern()),
    ])
}

/// Five scanned-style letter pages sharing one image XObject: the
/// multipage fixture.
fn gen_multi_pdf() -> Vec<u8> {
    let mut objects = vec![catalog_object(), page_tree_object(&[3, 4, 5, 6, 7])];
    // Page objects 3..=7 reference their own content streams 8..=12 and
    // the shared image object 13.
    for page_number in 3..=7 {
        objects.push(image_page_object(
            2,
            "0 0 612 792",
            None,
            page_number + 5,
            13,
        ));
    }
    for _ in 0..5 {
        objects.push(stream_object(&draw_image_content(612, 792)));
    }
    objects.push(image_xobject(&english_scan_pattern()));
    assemble(&objects)
}

fn generated_fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("gen_eng.pdf", gen_eng_pdf()),
        ("gen_hun.pdf", gen_hun_pdf()),
        ("gen_hybrid.pdf", gen_hybrid_pdf()),
        ("gen_rotated.pdf", gen_rotated_pdf()),
        ("gen_multi.pdf", gen_multi_pdf()),
    ]
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

// --- tests ---------------------------------------------------------------

#[test]
fn committed_fixtures_match_the_generator() {
    let regenerate = std::env::var(REGEN_ENV).is_ok_and(|value| value == "1");
    for (name, bytes) in generated_fixtures() {
        let path = fixture_path(name);
        if regenerate {
            fs::write(&path, &bytes)
                .unwrap_or_else(|err| panic!("cannot write {}: {err}", path.display()));
            eprintln!("regenerated {}", path.display());
        } else {
            let committed = fs::read(&path).unwrap_or_else(|err| {
                panic!(
                    "cannot read committed fixture {} ({err}); generate it with \
{REGEN_ENV}=1 cargo test --test fixtures_gen",
                    path.display()
                )
            });
            assert!(
                committed == bytes,
                "{name} differs from the generator output; regenerate it with \
{REGEN_ENV}=1 cargo test --test fixtures_gen"
            );
        }
    }
}

#[test]
fn generators_are_deterministic() {
    let first = generated_fixtures();
    let second = generated_fixtures();
    for ((name, bytes), (name_again, bytes_again)) in first.iter().zip(&second) {
        assert_eq!(name, name_again);
        assert_eq!(
            bytes, bytes_again,
            "{name} is not byte-for-byte reproducible"
        );
    }
}

#[test]
fn fixtures_stay_small() {
    for (name, bytes) in generated_fixtures() {
        assert!(
            bytes.len() < 100_000,
            "{name} is {} bytes; keep every fixture under 100 KB",
            bytes.len()
        );
    }
    let total: usize = generated_fixtures().iter().map(|(_, b)| b.len()).sum();
    assert!(
        total < 2 * 1024 * 1024,
        "new fixtures total {total} bytes; keep the fixture budget under 2 MB"
    );
}

#[test]
fn fixtures_are_loadable_and_shaped_as_documented() {
    let eng = Document::load_mem(&gen_eng_pdf()).expect("gen_eng.pdf loads");
    assert_eq!(eng.get_pages().len(), 1);
    assert_eq!(contents_streams(&eng, 0).len(), 1);
    assert_eq!(image_streams(&eng, 0).len(), 1);

    let hun = Document::load_mem(&gen_hun_pdf()).expect("gen_hun.pdf loads");
    assert_eq!(hun.get_pages().len(), 1);
    assert_eq!(image_streams(&hun, 0).len(), 1);

    let hybrid = Document::load_mem(&gen_hybrid_pdf()).expect("gen_hybrid.pdf loads");
    assert_eq!(hybrid.get_pages().len(), 2);
    // Page one must carry the genuine text layer ...
    let text = String::from_utf8_lossy(&contents_streams(&hybrid, 0)[0]).into_owned();
    assert!(text.contains("BT") && text.contains(HYBRID_TEXT));
    assert!(image_streams(&hybrid, 0).is_empty());
    // ... while page two is an image-only scan page.
    assert_eq!(image_streams(&hybrid, 1).len(), 1);
    assert!(
        String::from_utf8_lossy(&contents_streams(&hybrid, 1)[0]).contains("/Im0 Do"),
        "hybrid scan page must draw the image"
    );

    let rotated = Document::load_mem(&gen_rotated_pdf()).expect("gen_rotated.pdf loads");
    assert_eq!(rotated.get_pages().len(), 1);
    let rotated_page = page_dict(&rotated, 0);
    assert_eq!(
        rotated_page.get(b"Rotate").ok(),
        Some(&Object::Integer(90)),
        "rotated fixture must be tagged /Rotate 90"
    );
    assert_eq!(
        box_numbers(&rotated_page, b"MediaBox"),
        Some([0.0, 0.0, 792.0, 612.0]),
        "rotated fixture must use a landscape media box"
    );

    let multi = Document::load_mem(&gen_multi_pdf()).expect("gen_multi.pdf loads");
    assert_eq!(multi.get_pages().len(), 5);
    for page in 0..5 {
        assert_eq!(contents_streams(&multi, page).len(), 1);
        assert_eq!(image_streams(&multi, page).len(), 1);
    }
}

// --- lopdf helpers (same patterns as tests/overlay.rs) -------------------

fn page_dict(doc: &Document, page: usize) -> Dictionary {
    let id = *doc.get_pages().values().nth(page).unwrap();
    doc.get_object(id).unwrap().as_dict().unwrap().clone()
}

fn deref<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    doc.dereference(object).expect("dereference").1
}

fn contents_streams(doc: &Document, page: usize) -> Vec<Vec<u8>> {
    let dict = page_dict(doc, page);
    let contents = dict.get(b"Contents").expect("page has /Contents");
    match deref(doc, contents) {
        Object::Stream(stream) => vec![stream.content.clone()],
        Object::Array(entries) => entries
            .iter()
            .map(|entry| deref(doc, entry).as_stream().expect("content stream"))
            .map(|stream| stream.content.clone())
            .collect(),
        other => panic!("unexpected /Contents: {other:?}"),
    }
}

fn image_streams(doc: &Document, page: usize) -> Vec<Vec<u8>> {
    let dict = page_dict(doc, page);
    let resources = deref(doc, dict.get(b"Resources").expect("page has /Resources"));
    let Ok(xobjects) = resources.as_dict().unwrap().get(b"XObject") else {
        return Vec::new();
    };
    let xobjects = deref(doc, xobjects).as_dict().unwrap();
    xobjects
        .iter()
        .map(|(_, object)| {
            deref(doc, object)
                .as_stream()
                .expect("image xobject")
                .content
                .clone()
        })
        .collect()
}

/// Reads a `/MediaBox`-style entry as four numbers, tolerating the
/// Integer/Real distinction (see tests/overlay.rs).
fn box_numbers(page_dict: &Dictionary, key: &[u8]) -> Option<[f64; 4]> {
    let entries = match page_dict.get(key).ok()? {
        Object::Array(entries) => entries,
        _ => return None,
    };
    let mut numbers = [0.0; 4];
    for (slot, entry) in numbers.iter_mut().zip(entries) {
        *slot = match entry {
            Object::Integer(value) => *value as f64,
            Object::Real(value) => f64::from(*value),
            _ => return None,
        };
    }
    Some(numbers)
}
