//! Integration tests for the invisible-text overlay writer (issue #13).
//!
//! Ground-truth word boxes below were derived from the fixture generator
//! layout (`tests/fixtures/make_fixtures.py`) with the same DejaVu Sans
//! font and `PIL.ImageDraw.textbbox`, at each fixture's own 144 DPI (2 px
//! per point) in DISPLAY pixel space — the space `OcrPage` boxes live in.
//!
//! Every assertion is in-process: the merged output is parsed back with
//! lopdf and the overlay words (UTF-16BE hex strings) plus their `Tm`
//! positioning are compared against golden user-space coordinates, so a
//! plain `cargo test` is self-contained and deterministic on every OS.
//! (The original `pdftotext` assertions broke on the Windows CI runner:
//! poppler there exits non-zero for `-bbox` and hands out mojibake for
//! non-ASCII text — see issue #13.)

use chilli_pdf::ocr::{OcrLine, OcrPage, OcrWord, Rect};
use chilli_pdf::overlay::{OverlayPage, write_overlay};
use chilli_pdf::pdf::{PageBox, PageMetadata, PageRotation};
use lopdf::{Document, Object};
use std::path::{Path, PathBuf};

// --- ground truth (derived from tests/fixtures/make_fixtures.py) ---

const ENG_WORDS: &[(&str, i32, i32, i32, i32)] = &[
    ("Chilli", 144, 149, 212, 170),
    ("PDF", 221, 150, 276, 170),
    ("converts", 285, 151, 406, 170),
    ("scanned", 415, 149, 533, 170),
    ("documents", 541, 149, 697, 170),
    ("into", 706, 149, 760, 170),
    ("searchable", 769, 149, 922, 170),
    ("PDFs.", 931, 150, 1009, 170),
    ("The", 143, 199, 196, 220),
    ("quick", 205, 199, 281, 226),
    ("brown", 289, 199, 375, 220),
    ("fox", 384, 199, 427, 220),
    ("jumps", 435, 199, 521, 226),
    ("over", 530, 205, 593, 220),
    ("the", 601, 199, 647, 220),
    ("lazy", 656, 199, 712, 226),
    ("dog.", 721, 199, 783, 226),
    ("Searchable", 144, 249, 300, 270),
    ("text", 309, 251, 364, 270),
    ("can", 373, 255, 423, 270),
    ("be", 432, 249, 467, 270),
    ("selected", 476, 249, 594, 270),
    ("and", 604, 249, 657, 270),
    ("copied", 665, 249, 758, 276),
    ("from", 767, 249, 832, 270),
    ("the", 841, 249, 887, 270),
    ("output.", 896, 251, 997, 276),
    ("Offline", 144, 299, 236, 320),
    ("OCR", 245, 300, 306, 320),
    ("keeps", 315, 299, 397, 326),
    ("every", 406, 305, 485, 326),
    ("document", 494, 299, 635, 320),
    ("on", 644, 305, 679, 320),
    ("the", 688, 299, 734, 320),
    ("local", 743, 299, 808, 320),
    ("machine.", 817, 299, 946, 320),
    ("Tesseract", 143, 350, 275, 370),
    ("reads", 284, 349, 362, 370),
    ("the", 371, 349, 417, 370),
    ("page;", 425, 355, 504, 376),
    ("PDFium", 514, 349, 619, 370),
    ("renders", 628, 349, 735, 370),
    ("it;", 744, 349, 772, 374),
    ("the", 781, 349, 827, 370),
    ("overlay", 836, 349, 940, 376),
    ("aligns.", 949, 349, 1041, 376),
];

const HUN_WORDS: &[(&str, i32, i32, i32, i32)] = &[
    ("A", 144, 152, 170, 180),
    ("gyors", 182, 158, 287, 188),
    ("barna", 299, 151, 409, 180),
    ("róka", 421, 150, 504, 180),
    ("átugrik", 516, 150, 651, 188),
    ("a", 663, 158, 686, 180),
    ("lusta", 698, 151, 791, 180),
    ("kutya", 803, 151, 909, 188),
    ("fölött.", 920, 151, 1032, 180),
    ("Tűzön", 143, 218, 259, 248),
    ("át", 271, 218, 309, 248),
    ("visz", 321, 219, 394, 248),
    ("az", 406, 226, 449, 248),
    ("út,", 461, 218, 512, 252),
    ("de", 524, 219, 572, 248),
    ("csak", 584, 219, 670, 248),
    ("óvatosan.", 682, 218, 868, 248),
    ("Őssel", 144, 281, 247, 316),
    ("az", 260, 294, 303, 316),
    ("őz", 315, 286, 358, 316),
    ("gyakran", 370, 287, 525, 324),
    ("látható", 537, 286, 671, 316),
    ("a", 683, 294, 706, 316),
    ("szélén", 719, 286, 840, 316),
    ("az", 852, 294, 895, 316),
    ("erdőnek.", 907, 286, 1074, 316),
    ("Az", 144, 356, 190, 384),
    ("űrhajósok", 202, 354, 387, 392),
    ("űrruhát", 399, 354, 540, 384),
    ("viseltek", 552, 355, 699, 384),
    ("az", 712, 362, 755, 384),
    ("űrállomáson.", 767, 354, 1014, 384),
    ("Árvíztűrő", 144, 417, 316, 452),
    ("tükörfúrógép.", 329, 422, 587, 460),
];

const ROTATED_WORDS: &[(&str, i32, i32, i32, i32)] = &[
    ("This", 143, 151, 218, 178),
    ("page", 229, 158, 319, 185),
    ("is", 330, 151, 359, 178),
    ("stored", 371, 151, 485, 178),
    ("sideways", 496, 151, 661, 185),
    ("and", 673, 151, 741, 178),
    ("marked", 752, 151, 889, 178),
    ("rotated.", 900, 151, 1043, 178),
    ("The", 143, 215, 211, 242),
    ("quick", 222, 215, 318, 249),
    ("brown", 330, 215, 441, 242),
    ("fox", 453, 215, 508, 242),
    ("appears", 519, 222, 665, 249),
    ("upright", 676, 215, 806, 249),
    ("after", 818, 215, 903, 242),
    ("rotation.", 914, 215, 1067, 242),
    ("Rendering", 144, 279, 328, 313),
    ("applies", 339, 279, 468, 313),
    ("the", 479, 279, 538, 306),
    ("rotation", 550, 279, 691, 306),
    ("attribute", 703, 279, 860, 306),
    ("correctly.", 871, 279, 1035, 313),
];

const CROPBOX_WORDS: &[(&str, i32, i32, i32, i32)] = &[
    ("This", 71, 79, 146, 106),
    ("page", 157, 86, 247, 113),
    ("has", 258, 79, 322, 106),
    ("a", 333, 86, 355, 106),
    ("crop", 367, 86, 446, 113),
    ("box", 457, 79, 522, 106),
    ("inset", 534, 79, 622, 106),
    ("from", 633, 79, 717, 106),
    ("the", 728, 79, 787, 106),
    ("media", 799, 79, 911, 106),
    ("box.", 922, 79, 999, 106),
    ("The", 71, 143, 139, 170),
    ("crop", 150, 150, 229, 177),
    ("box", 241, 143, 306, 170),
    ("trims", 317, 143, 410, 170),
    ("the", 421, 143, 480, 170),
    ("outer", 492, 144, 588, 170),
    ("half", 599, 143, 668, 170),
    ("inch", 678, 143, 753, 170),
    ("of", 765, 143, 801, 170),
    ("paper.", 811, 150, 924, 177),
    ("Text", 71, 208, 145, 234),
    ("must", 156, 208, 247, 234),
    ("align", 258, 207, 346, 241),
    ("inside", 358, 207, 465, 234),
    ("the", 476, 207, 535, 234),
    ("cropped", 546, 207, 693, 241),
    ("visible", 704, 207, 819, 234),
    ("area.", 831, 214, 923, 234),
];

// --- helpers ---

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
/// Letter crop box (also the media box of the plain fixtures).
const LETTER: PageBox = PageBox {
    left: 0.0,
    bottom: 0.0,
    right: 612.0,
    top: 792.0,
};
/// The rotated fixture's landscape media box [0 0 792 612].
const LANDSCAPE: PageBox = PageBox {
    left: 0.0,
    bottom: 0.0,
    right: 792.0,
    top: 612.0,
};
/// The cropbox fixture's inset [36 36 576 756].
const INSET: PageBox = PageBox {
    left: 36.0,
    bottom: 36.0,
    right: 576.0,
    top: 756.0,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(FIXTURES).join(name)
}

fn glyphless_font() -> Vec<u8> {
    std::fs::read(fixture("pdf.ttf")).expect("glyphless font fixture is committed")
}

fn ocr_page(table: &[(&str, i32, i32, i32, i32)], width: u32, height: u32, dpi: u32) -> OcrPage {
    let words = table
        .iter()
        .map(|(text, l, t, r, b)| OcrWord {
            text: (*text).into(),
            confidence: 90.0,
            rect: Rect::new(*l, *t, *r, *b),
        })
        .collect();
    OcrPage {
        width,
        height,
        dpi,
        lines: vec![OcrLine {
            rect: Rect::new(0, 0, width as i32, height as i32),
            words,
        }],
    }
}

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("tmpdir");
    dir.join(name)
}

/// Build the merged, searchable version of one fixture.
fn merged(
    input: &str,
    out_name: &str,
    words: &[(&str, i32, i32, i32, i32)],
    page_px: (u32, u32),
    metadata: PageMetadata,
) -> PathBuf {
    let ocr = ocr_page(words, page_px.0, page_px.1, 144);
    let pages = vec![OverlayPage { ocr, metadata }];
    let output = tmp(out_name);
    write_overlay(&fixture(input), &output, &pages, &glyphless_font())
        .unwrap_or_else(|e| panic!("overlaying {input}: {e}"));
    output
}

// --- in-Rust overlay verification (lopdf only, no external tools) ---

/// One word of the appended overlay, parsed back out of the merged PDF:
/// the decoded text plus the `Tf`/`Tz`/`Tm` operands that position it.
#[derive(Debug)]
struct OverlayWord {
    text: String,
    font_size: f64,
    horizontal_scale: f64,
    matrix: [f64; 6],
}

/// Parses the appended overlay content stream of a merged fixture back
/// into its words.
///
/// The overlay words are written as UTF-16BE hex strings, so they can be
/// decoded exactly on every OS — no shell, no tool-dependent encoding.
/// This replaces the earlier `pdftotext` assertions, which broke on the
/// Windows runner (issue #13): poppler there exits non-zero for `-bbox`
/// and mangles non-ASCII output.
fn overlay_words(pdf: &Path) -> Vec<OverlayWord> {
    let doc = Document::load(pdf).expect("merged output loads");
    let page = page_dict(&doc, 0);
    let streams = contents_streams(&doc, &page);
    assert_eq!(
        streams.len(),
        2,
        "expected the original stream plus one overlay stream"
    );
    let overlay = String::from_utf8(streams[1].content.clone())
        .expect("overlay ops are ASCII plus hex strings");
    let lines: Vec<&str> = overlay.lines().collect();
    assert_eq!(lines.first(), Some(&"q"), "overlay must open with q");
    assert_eq!(lines.last(), Some(&"Q"), "overlay must close with Q");
    assert!(
        lines.len() >= 2,
        "overlay must at least wrap its words in q … Q"
    );
    lines[1..lines.len() - 1]
        .iter()
        .map(|line| parse_overlay_op(line))
        .collect()
}

/// Parses one overlay operator:
/// `BT 3 Tr /ChilliOcrF1 <fs> Tf <tz> Tz <a b c d e f> Tm <hex> Tj ET`
fn parse_overlay_op(line: &str) -> OverlayWord {
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields.len(), 18, "unexpected operator shape: {line:?}");
    assert_eq!(fields[0], "BT");
    assert_eq!(
        fields[1], "3",
        "text render mode must stay invisible (3): {line:?}"
    );
    assert_eq!(fields[2], "Tr");
    assert_eq!(fields[3], "/ChilliOcrF1");
    assert_eq!(fields[5], "Tf");
    assert_eq!(fields[7], "Tz");
    assert_eq!(fields[14], "Tm");
    assert_eq!(fields[16], "Tj");
    assert_eq!(fields[17], "ET");
    let number = |index: usize| {
        fields[index]
            .parse::<f64>()
            .unwrap_or_else(|_| panic!("operand {index} is not a number: {line:?}"))
    };
    OverlayWord {
        text: decode_hex_text(fields[15]),
        font_size: number(4),
        horizontal_scale: number(6),
        matrix: [
            number(8),
            number(9),
            number(10),
            number(11),
            number(12),
            number(13),
        ],
    }
}

/// Decodes the `<UTF-16BE hex>` operand of a `Tj` back into text,
/// stripping the one trailing space the writer appends.
fn decode_hex_text(field: &str) -> String {
    let hex = field
        .strip_prefix('<')
        .and_then(|h| h.strip_suffix('>'))
        .expect("hex string delimiters");
    assert_eq!(
        hex.len() % 4,
        0,
        "each UTF-16 code unit is 4 hex digits: {field:?}"
    );
    let units: Vec<u16> = (0..hex.len())
        .step_by(4)
        .map(|i| u16::from_str_radix(&hex[i..i + 4], 16).expect("hex code unit"))
        .collect();
    let mut text = String::from_utf16(&units).expect("valid UTF-16BE text");
    assert_eq!(text.pop(), Some(' '), "exactly one trailing space");
    text
}

/// Golden expectations for one fixture's overlay words, derived straight
/// from the spike's display→user-space table (`docs/spike-report.md`
/// §3) rather than from the production code, so this stays a real
/// cross-check: with the display size in POINTS (`display_pt`), the
/// crop-box origin and the page rotation, each word's baseline anchor is
/// the display-space bottom-left corner mapped back through the inverse
/// rotation. Font size is the OCR box height in points and `Tz` stretches
/// the glyphless font's fixed 0.5 em advance to the box width.
fn expected_overlay_words(
    words: &[(&str, i32, i32, i32, i32)],
    display_pt: (f64, f64),
    crop: PageBox,
    rotation: PageRotation,
) -> Vec<OverlayWord> {
    const PT_PER_PX: f64 = 0.5; // the fixtures are 144 DPI
    let (w, h) = display_pt;
    words
        .iter()
        .map(|&(text, left, top, right, bottom)| {
            let (dx, dy) = (f64::from(left) * PT_PER_PX, f64::from(bottom) * PT_PER_PX);
            let font_size = f64::from(bottom - top) * PT_PER_PX;
            let codes = text.encode_utf16().count() as f64;
            let horizontal_scale =
                100.0 * f64::from(right - left) * PT_PER_PX / (0.5 * font_size * codes);
            let (matrix, a, b) = match rotation {
                PageRotation::None => ([1.0, 0.0, 0.0, 1.0], dx, h - dy),
                PageRotation::Degrees90 => ([0.0, 1.0, -1.0, 0.0], dy, dx),
                PageRotation::Degrees180 => ([-1.0, 0.0, 0.0, -1.0], w - dx, dy),
                PageRotation::Degrees270 => ([0.0, -1.0, 1.0, 0.0], h - dy, w - dx),
            };
            OverlayWord {
                text: text.to_string(),
                font_size,
                horizontal_scale,
                matrix: [
                    matrix[0],
                    matrix[1],
                    matrix[2],
                    matrix[3],
                    crop.left + a,
                    crop.bottom + b,
                ],
            }
        })
        .collect()
}

/// Parsed words must match the golden expectations exactly: text is the
/// Hungarian ő/ű round-trip, and the numbers may only differ by the
/// two-decimal formatting of the operators (±0.006).
fn assert_overlay_matches(actual: &[OverlayWord], expected: &[OverlayWord]) {
    assert_eq!(actual.len(), expected.len(), "overlay word count");
    for (got, want) in actual.iter().zip(expected) {
        assert_eq!(got.text, want.text, "decoded overlay text");
        assert!(
            (got.font_size - want.font_size).abs() < 0.006,
            "font size: got {got:?}, want {want:?}"
        );
        assert!(
            (got.horizontal_scale - want.horizontal_scale).abs() < 0.006,
            "Tz: got {got:?}, want {want:?}"
        );
        for (got, want) in got.matrix.iter().zip(want.matrix) {
            assert!((got - want).abs() < 0.006, "Tm: got {got:?}, want {want:?}");
        }
    }
}

// --- text round-trip tests ---

#[test]
fn eng_scan_words_are_selectable() {
    let pdf = merged(
        "eng_scan.pdf",
        "eng_merged.pdf",
        ENG_WORDS,
        (1224, 1584),
        PageMetadata {
            rotation: PageRotation::None,
            crop_box: LETTER,
        },
    );
    let words = overlay_words(&pdf);
    let decoded: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let expected: Vec<&str> = ENG_WORDS.iter().map(|w| w.0).collect();
    assert_eq!(
        decoded, expected,
        "the overlay must carry every word of the page, in order"
    );
}

#[test]
fn hun_scan_preserves_double_acute_diacritics() {
    let pdf = merged(
        "hun_scan.pdf",
        "hun_merged.pdf",
        HUN_WORDS,
        (1224, 1584),
        PageMetadata {
            rotation: PageRotation::None,
            crop_box: LETTER,
        },
    );
    // Decoding the UTF-16BE hex strings back must reproduce ő/ű exactly
    // — this is the copy-paste round-trip, with no tool encoding in
    // between (the old `pdftotext` variant read mojibake on Windows).
    let words = overlay_words(&pdf);
    let decoded: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let expected: Vec<&str> = HUN_WORDS.iter().map(|w| w.0).collect();
    assert_eq!(
        decoded, expected,
        "Hungarian words must round-trip through the overlay byte-exactly"
    );
}

#[test]
fn rotated_scan_words_read_upright_after_rotation() {
    // Display pixel space is the upright portrait page (612×792 pt at
    // 144 DPI = 1224×1584 px) even though the stored image is sideways.
    let pdf = merged(
        "rotated_scan.pdf",
        "rotated_merged.pdf",
        ROTATED_WORDS,
        (1224, 1584),
        PageMetadata {
            rotation: PageRotation::Degrees90,
            crop_box: LANDSCAPE,
        },
    );
    let words = overlay_words(&pdf);
    let decoded: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let expected: Vec<&str> = ROTATED_WORDS.iter().map(|w| w.0).collect();
    assert_eq!(
        decoded, expected,
        "the overlay must carry every word of the rotated page, in order"
    );
}

#[test]
fn cropbox_scan_words_align_inside_the_cropped_area() {
    // Display pixel space is the crop region itself: 540×720 pt at 144 DPI
    // = 1080×1440 px; ground-truth boxes are offset by 36 pt (72 px).
    let pdf = merged(
        "cropbox_scan.pdf",
        "cropbox_merged.pdf",
        CROPBOX_WORDS,
        (1080, 1440),
        PageMetadata {
            rotation: PageRotation::None,
            crop_box: INSET,
        },
    );
    let words = overlay_words(&pdf);
    let decoded: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let expected: Vec<&str> = CROPBOX_WORDS.iter().map(|w| w.0).collect();
    assert_eq!(
        decoded, expected,
        "the overlay must carry every word of the cropped page, in order"
    );
}

// --- position tests ---

#[test]
fn eng_overlay_sits_on_the_words() {
    let pdf = merged(
        "eng_scan.pdf",
        "eng_position.pdf",
        ENG_WORDS,
        (1224, 1584),
        PageMetadata {
            rotation: PageRotation::None,
            crop_box: LETTER,
        },
    );
    assert_overlay_matches(
        &overlay_words(&pdf),
        &expected_overlay_words(ENG_WORDS, (612.0, 792.0), LETTER, PageRotation::None),
    );
}

#[test]
fn rotated_overlay_transposes_correctly() {
    let pdf = merged(
        "rotated_scan.pdf",
        "rotated_position.pdf",
        ROTATED_WORDS,
        (1224, 1584),
        PageMetadata {
            rotation: PageRotation::Degrees90,
            crop_box: LANDSCAPE,
        },
    );
    // The stored image is sideways; display pixel space is the upright
    // portrait page. Every word's Tm must transpose through the inverse
    // /Rotate 90 mapping (spike §3): user = (dy_bottom, dx_left) in the
    // landscape crop box, with the 90° rotation matrix `0 1 -1 0`.
    assert_overlay_matches(
        &overlay_words(&pdf),
        &expected_overlay_words(
            ROTATED_WORDS,
            (612.0, 792.0),
            LANDSCAPE,
            PageRotation::Degrees90,
        ),
    );
}

#[test]
fn cropbox_overlay_positions_are_media_absolute() {
    let pdf = merged(
        "cropbox_scan.pdf",
        "cropbox_position.pdf",
        CROPBOX_WORDS,
        (1080, 1440),
        PageMetadata {
            rotation: PageRotation::None,
            crop_box: INSET,
        },
    );
    // PDF user space is media-absolute: a crop box never moves the
    // origin, so the overlay must add the crop-box origin (36, 36) to
    // every display-space anchor itself. Display space here is the
    // 540×720 pt crop region.
    assert_overlay_matches(
        &overlay_words(&pdf),
        &expected_overlay_words(CROPBOX_WORDS, (540.0, 720.0), INSET, PageRotation::None),
    );
}

// --- appearance-preservation tests (lopdf only, always run) ---

fn page_dict(doc: &Document, page: usize) -> lopdf::Dictionary {
    let id = *doc.get_pages().values().nth(page).unwrap();
    doc.get_object(id).unwrap().as_dict().unwrap().clone()
}

fn deref<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    doc.dereference(object).expect("dereference").1
}

fn contents_streams<'a>(
    doc: &'a Document,
    page_dict: &'a lopdf::Dictionary,
) -> Vec<&'a lopdf::Stream> {
    let contents = page_dict.get(b"Contents").unwrap();
    match deref(doc, contents) {
        Object::Stream(s) => vec![s],
        Object::Array(entries) => entries
            .iter()
            .map(|e| deref(doc, e).as_stream().expect("content stream"))
            .collect(),
        other => panic!("unexpected /Contents: {other:?}"),
    }
}

fn image_streams(doc: &Document, page_dict: &lopdf::Dictionary) -> Vec<Vec<u8>> {
    let resources = deref(doc, page_dict.get(b"Resources").expect("resources"));
    if let Ok(xobjects) = resources.as_dict().unwrap().get(b"XObject") {
        let xobjects = deref(doc, xobjects).as_dict().unwrap();
        return xobjects
            .iter()
            .map(|(_, obj)| {
                deref(doc, obj)
                    .as_stream()
                    .expect("image xobject")
                    .content
                    .clone()
            })
            .collect();
    }
    Vec::new()
}

/// Reads a `/MediaBox`-style entry as four numbers, tolerating the
/// Integer/Real distinction: lopdf's writer normalises `612.0` to `612`, so
/// an exactly-preserved box still changes `Object` variants on save + reload.
fn box_numbers(page_dict: &lopdf::Dictionary, key: &[u8]) -> Option<[f64; 4]> {
    let entries = match page_dict.get(key).ok()? {
        Object::Array(entries) => entries,
        _ => return None,
    };
    let mut numbers = [0.0; 4];
    for (slot, entry) in numbers.iter_mut().zip(entries) {
        *slot = match entry {
            Object::Integer(i) => *i as f64,
            Object::Real(x) => f64::from(*x),
            _ => return None,
        };
    }
    Some(numbers)
}

/// One fixture's inputs for the appearance-preservation test:
/// input file, output name, ground-truth words, OCR page size in pixels,
/// and the page metadata.
type PreservationCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, i32, i32, i32, i32)],
    (u32, u32),
    PageMetadata,
);

#[test]
fn merged_output_preserves_the_original_appearance() {
    let cases: &[PreservationCase] = &[
        (
            "eng_scan.pdf",
            "eng_preserve.pdf",
            ENG_WORDS,
            (1224, 1584),
            PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        ),
        (
            "hun_scan.pdf",
            "hun_preserve.pdf",
            HUN_WORDS,
            (1224, 1584),
            PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        ),
        (
            "rotated_scan.pdf",
            "rotated_preserve.pdf",
            ROTATED_WORDS,
            (1224, 1584),
            PageMetadata {
                rotation: PageRotation::Degrees90,
                crop_box: LANDSCAPE,
            },
        ),
        (
            "cropbox_scan.pdf",
            "cropbox_preserve.pdf",
            CROPBOX_WORDS,
            (1080, 1440),
            PageMetadata {
                rotation: PageRotation::None,
                crop_box: INSET,
            },
        ),
    ];
    for (input, out_name, words, px, metadata) in cases {
        let output = merged(input, out_name, words, *px, *metadata);

        let before = Document::load(fixture(input)).unwrap();
        let after = Document::load(&output).unwrap();
        let before_page = page_dict(&before, 0);
        let after_page = page_dict(&after, 0);

        // Page geometry and rotation are untouched. Boxes are compared
        // numerically (see `box_numbers`); /Rotate survives exactly.
        for key in [b"MediaBox".as_slice(), b"CropBox".as_slice()] {
            assert_eq!(
                box_numbers(&before_page, key),
                box_numbers(&after_page, key),
                "{input}: {key:?} changed"
            );
        }
        assert_eq!(
            before_page.get(b"Rotate").ok(),
            after_page.get(b"Rotate").ok(),
            "{input}: /Rotate changed"
        );
        assert!(
            box_numbers(&before_page, b"MediaBox").is_some(),
            "{input}: fixture must have a /MediaBox"
        );

        // The original content stream is still there, still first, with
        // byte-identical (still-encoded) content; the overlay was appended.
        let before_contents = contents_streams(&before, &before_page);
        let after_contents = contents_streams(&after, &after_page);
        assert_eq!(
            before_contents.len(),
            1,
            "{input}: expected a single stream"
        );
        assert_eq!(after_contents.len(), 2, "{input}: overlay stream missing");
        assert_eq!(
            before_contents[0].content, after_contents[0].content,
            "{input}: original content bytes changed"
        );
        assert_eq!(
            before_contents[0].dict, after_contents[0].dict,
            "{input}: original content dict changed"
        );

        // The scanned image is untouched.
        assert_eq!(
            image_streams(&before, &before_page),
            image_streams(&after, &after_page),
            "{input}: image stream changed"
        );

        // The overlay stream is uncompressed and wrapped in q … Q.
        let overlay = &after_contents[1];
        assert!(overlay.dict.get(b"Filter").is_err());
        let text = String::from_utf8(overlay.content.clone()).unwrap();
        assert!(
            text.starts_with("q\nBT 3 Tr /ChilliOcrF1"),
            "{input}: {text:?}"
        );
        assert!(text.ends_with("Q\n"));

        // The glyphless font is wired into the page resources, so real
        // viewers can decode the overlay (the overlay words reference it
        // as /ChilliOcrF1 and it must resolve to a Type0 font).
        let resources = deref(&after, after_page.get(b"Resources").unwrap());
        let fonts = deref(&after, resources.as_dict().unwrap().get(b"Font").unwrap());
        let font = deref(
            &after,
            fonts
                .as_dict()
                .unwrap()
                .get(b"ChilliOcrF1")
                .unwrap_or_else(|_| panic!("{input}: /ChilliOcrF1 is missing from /Resources")),
        );
        assert_eq!(
            font.as_dict()
                .unwrap()
                .get(b"Subtype")
                .unwrap()
                .as_name()
                .unwrap(),
            b"Type0",
            "{input}: overlay font must be a Type0 font"
        );
    }
}

// --- error-path tests (lopdf only, always run) ---

#[test]
fn encrypted_input_is_refused() {
    let err = write_overlay(
        &fixture("encrypted_scan.pdf"),
        &tmp("never.pdf"),
        &[OverlayPage {
            ocr: OcrPage::empty(1224, 1584, 144),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        }],
        &glyphless_font(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        chilli_pdf::overlay::OverlayError::EncryptedUnsupported { .. }
    ));
}

#[test]
fn input_needing_xref_repair_is_refused_as_unreadable() {
    // A startxref offset that points into the file header: lopdf can
    // only "load" this by reconstructing the cross-reference table from
    // raw bytes (last-resort recovery). The overlay refuses such files
    // as unreadable instead of editing objects through a guessed xref.
    let broken_xref = b"%PDF-1.4\n\
        1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
        xref\n0 1\n0000000000 65535 f\n\
        trailer\n<< /Size 1 /Root 1 0 R >>\n\
        startxref\n9\n\
        %%EOF\n";
    let input = tmp("broken_xref_input.pdf");
    std::fs::write(&input, broken_xref).expect("write broken input");
    let err = write_overlay(
        &input,
        &tmp("never.pdf"),
        &[OverlayPage {
            ocr: OcrPage::empty(1224, 1584, 144),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        }],
        &glyphless_font(),
    )
    .unwrap_err();
    assert!(
        matches!(err, chilli_pdf::overlay::OverlayError::ReadFailed { .. }),
        "got {err:?}"
    );
}

#[test]
fn page_count_must_match_the_document() {
    let two_pages = vec![
        OverlayPage {
            ocr: OcrPage::empty(1224, 1584, 144),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER
            },
        };
        2
    ];
    let err = write_overlay(
        &fixture("eng_scan.pdf"),
        &tmp("never.pdf"),
        &two_pages,
        &glyphless_font(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            chilli_pdf::overlay::OverlayError::PageCountMismatch { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn mispaired_ocr_geometry_is_rejected() {
    // A 100×100 px "OCR page" is not letter at 144 DPI.
    let err = write_overlay(
        &fixture("eng_scan.pdf"),
        &tmp("never.pdf"),
        &[OverlayPage {
            ocr: OcrPage::empty(100, 100, 144),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        }],
        &glyphless_font(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            chilli_pdf::overlay::OverlayError::OcrGeometryMismatch { .. }
        ),
        "got {err:?}"
    );
}

#[test]
fn non_font_bytes_are_rejected() {
    let err = write_overlay(
        &fixture("eng_scan.pdf"),
        &tmp("never.pdf"),
        &[OverlayPage {
            ocr: OcrPage::empty(1224, 1584, 144),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        }],
        b"",
    )
    .unwrap_err();
    assert!(
        matches!(err, chilli_pdf::overlay::OverlayError::InvalidFont),
        "got {err:?}"
    );
}

#[test]
fn empty_ocr_leaves_the_page_untouched() {
    let output = tmp("empty_ocr.pdf");
    write_overlay(
        &fixture("eng_scan.pdf"),
        &output,
        &[OverlayPage {
            ocr: OcrPage::empty(1224, 1584, 144),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        }],
        &glyphless_font(),
    )
    .unwrap();

    let before = Document::load(fixture("eng_scan.pdf")).unwrap();
    let after = Document::load(&output).unwrap();
    let before_page = page_dict(&before, 0);
    let after_page = page_dict(&after, 0);
    assert_eq!(
        contents_streams(&before, &before_page).len(),
        contents_streams(&after, &after_page).len(),
        "empty OCR must not append anything"
    );
    assert!(
        !after_page
            .get(b"Resources")
            .is_ok_and(|r| { deref(&after, r).as_dict().unwrap().has(b"Font") }),
        "empty OCR must not add font resources"
    );
}
