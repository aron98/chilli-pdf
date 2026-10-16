//! End-to-end OCR tests against the committed Hungarian fixture.
//!
//! These require a system Tesseract 5 (library + `eng`/`hun` traineddata).
//! When either is missing the tests print a skip notice and pass, so CI
//! runners without the native dependency stay green.

use std::io::Cursor;

use chilli_pdf::language::LanguagePreset;
use chilli_pdf::ocr::{
    OcrEngine, OcrImage, OcrPage, OcrPaths, OcrWord, PixelFormat, SystemOcrPaths,
};

/// The committed fixture: two 300 DPI grayscale Hungarian lines containing
/// both double-acute letters — "árvíztűrő tükörfúrógép" and "gőzgép és űrhajó".
const FIXTURE: &[u8] = include_bytes!("fixtures/hungarian_ou.png");

/// Decode the fixture PNG into raw grayscale pixels + dimensions.
fn decode_fixture() -> (Vec<u8>, u32, u32, PixelFormat) {
    let decoder = png::Decoder::new(Cursor::new(FIXTURE));
    let mut reader = decoder.read_info().expect("fixture PNG is valid");
    let capacity = reader.output_buffer_size().expect("fixture fits in memory");
    let mut buffer = vec![0u8; capacity];
    let info = reader.next_frame(&mut buffer).expect("fixture PNG decodes");
    assert_eq!(
        (info.color_type, info.bit_depth),
        (png::ColorType::Grayscale, png::BitDepth::Eight),
        "fixture must be 8-bit grayscale"
    );
    buffer.truncate(info.buffer_size());
    (buffer, info.width, info.height, PixelFormat::Gray)
}

/// Resolve a system Tesseract that can serve `preset`, or skip the calling
/// test by returning `None`.
fn system_paths(preset: LanguagePreset) -> Option<SystemOcrPaths> {
    let paths = match SystemOcrPaths::new() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("skipping OCR test ({preset:?}): {err}");
            return None;
        }
    };
    let dir = paths.tessdata_dir().expect("resolved tessdata");
    for code in preset.tessdata_codes() {
        if !dir.join(format!("{code}.traineddata")).is_file() {
            eprintln!("skipping OCR test ({preset:?}): {code}.traineddata missing");
            return None;
        }
    }
    Some(paths)
}

fn assert_structure(page: &OcrPage, width: u32, height: u32) {
    assert_eq!((page.width, page.height), (width, height));
    assert!(!page.lines.is_empty(), "fixture contains text");

    let words: Vec<&OcrWord> = page.words().collect();
    assert!(!words.is_empty(), "words were recognised");

    for word in &words {
        assert!(!word.text.is_empty(), "no empty words survive filtering");
        assert!(
            (0.0..=100.0).contains(&word.confidence),
            "confidence {} out of range",
            word.confidence
        );
        assert!(
            word.rect.left >= 0 && word.rect.top >= 0,
            "word rect off the top-left"
        );
        assert!(
            word.rect.right <= width as i32 && word.rect.bottom <= height as i32,
            "word rect beyond the page"
        );
        assert!(
            word.rect.width() > 0 && word.rect.height() > 0,
            "zero-size word rect"
        );
    }

    for line in &page.lines {
        assert!(!line.words.is_empty(), "no empty lines survive filtering");
        assert!(
            line.rect.width() > 0 || line.rect.height() > 0,
            "line has a non-empty rect"
        );
    }
}

#[test]
fn hungarian_preserves_double_acute() {
    let Some(paths) = system_paths(LanguagePreset::Hungarian) else {
        return;
    };
    let (data, width, height, format) = decode_fixture();
    let image = OcrImage::new(&data, width, height, format);

    let mut engine = OcrEngine::new(LanguagePreset::Hungarian, &paths).expect("engine initialises");
    assert_eq!(engine.preset(), LanguagePreset::Hungarian);

    let page = engine.recognize_page(&image).expect("recognition succeeds");
    assert_structure(&page, width, height);

    let text = page.text();
    assert!(
        text.contains('ő'),
        "expected ő in OCR output, got: {text:?}"
    );
    assert!(
        text.contains('ű'),
        "expected ű in OCR output, got: {text:?}"
    );

    // The engine is reusable: a second recognition on the same engine must
    // also preserve the diacritics (exercises Clear between recognitions).
    let again = engine
        .recognize_page(&image)
        .expect("second recognition succeeds");
    let again_text = again.text();
    assert!(
        again_text.contains('ő') && again_text.contains('ű'),
        "second pass lost diacritics"
    );
}

#[test]
fn combined_preserves_double_acute() {
    let Some(paths) = system_paths(LanguagePreset::EnglishHungarian) else {
        return;
    };
    let (data, width, height, format) = decode_fixture();
    let image = OcrImage::new(&data, width, height, format);

    let mut engine =
        OcrEngine::new(LanguagePreset::EnglishHungarian, &paths).expect("engine initialises");
    let page = engine.recognize_page(&image).expect("recognition succeeds");
    assert_structure(&page, width, height);
    assert!(!page.is_empty());

    let text = page.text();
    assert!(
        text.contains('ő') && text.contains('ű'),
        "expected ő and ű in combined-mode output, got: {text:?}"
    );
}

#[test]
fn english_runs_and_produces_structure() {
    let Some(paths) = system_paths(LanguagePreset::English) else {
        return;
    };
    let (data, width, height, format) = decode_fixture();
    let image = OcrImage::new(&data, width, height, format);

    let mut engine = OcrEngine::new(LanguagePreset::English, &paths).expect("engine initialises");
    let page = engine.recognize_page(&image).expect("recognition succeeds");
    assert_structure(&page, width, height);
    // English traineddata misreads Hungarian, but the engine must still emit words.
    assert!(
        !page.is_empty(),
        "english preset should still produce words"
    );
}
