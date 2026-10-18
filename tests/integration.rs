//! End-to-end integration tests for the conversion pipeline.
//!
//! These tests drive the real production path — validation, page rendering,
//! OCR orchestration, the invisible lopdf overlay, and the transactional
//! publish — through the public [`Converter`] API. The PDF engine and the
//! OCR recognizer are the crate's mocks (see `src/pdf/mock.rs` and the
//! scripted recognizers below), so the tests need no pdfium, no system OCR,
//! and no network. The input fixtures are the deterministic synthetic PDFs
//! generated and verified by `tests/fixtures_gen.rs`.
//!
//! Two facts make the assertions meaningful:
//! * `MockEngine` reports page specs that mirror the committed fixtures
//!   (page count and geometry), so `LopdfOverlay` reads the real fixture
//!   bytes and merges into them, exactly like production.
//! * The scripted recognizers return OCR pages sized like a real 300 DPI
//!   letter render (2550×3300), which is what the overlay layout expects.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chilli_pdf::converter::{Converter, Job, WorkerEvent};
use chilli_pdf::language::LanguagePreset;
use chilli_pdf::ocr::{OcrError, OcrImage, OcrLine, OcrPage, OcrWord, Rect};
use chilli_pdf::output_path::{Conflict, check_output, default_output_path};
use chilli_pdf::pdf::{MockEngine, MockPageSpec, PageBox, PageRotation, PdfEngine};
use chilli_pdf::transactional::is_temp_file_name;
use chilli_pdf::validation::{DEFAULT_MAX_SIZE_BYTES, ValidationConfig, validate_bytes};
use chilli_pdf::worker::{LopdfOverlay, OcrFactory, PageRecognizer, PipelineConverter};
use lopdf::{Dictionary, Document, Object};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn scratch(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("integration-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn glyphless_font() -> Vec<u8> {
    fs::read(fixture("pdf.ttf")).expect("read pdf.ttf")
}

/// A scripted recognizer: it returns the same OCR page regardless of the
/// image it is handed, and records every call so tests can assert which
/// pages the pipeline decided to OCR.
#[derive(Clone)]
struct ScriptedRecognizer {
    page: OcrPage,
    calls: Arc<Mutex<Vec<String>>>,
    delay: Duration,
}

impl PageRecognizer for ScriptedRecognizer {
    fn recognize(&mut self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError> {
        std::thread::sleep(self.delay);
        let mut calls = self.calls.lock().expect("calls lock");
        calls.push(format!(
            "{}x{}@{}",
            image.width(),
            image.height(),
            image.dpi()
        ));
        Ok(self.page.clone())
    }
}

fn scripted_ocr(page: OcrPage) -> (OcrFactory, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory = Arc::new(move |_language: LanguagePreset| {
        Ok(Box::new(ScriptedRecognizer {
            page: page.clone(),
            calls: Arc::clone(&factory_calls),
            delay: Duration::ZERO,
        }) as Box<dyn PageRecognizer>)
    });
    (factory, calls)
}

fn delayed_ocr(page: OcrPage, delay: Duration) -> (OcrFactory, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory = Arc::new(move |_language: LanguagePreset| {
        Ok(Box::new(ScriptedRecognizer {
            page: page.clone(),
            calls: Arc::clone(&factory_calls),
            delay,
        }) as Box<dyn PageRecognizer>)
    });
    (factory, calls)
}

/// A [`PdfEngine`] wrapper that counts how many documents were opened, so
/// tests can prove validation short-circuits before the engine runs.
#[derive(Clone)]
struct CountingEngine {
    inner: MockEngine,
    opens: Arc<AtomicUsize>,
}

impl PdfEngine for CountingEngine {
    fn open_document<'s>(
        &'s self,
        path: &Path,
    ) -> Result<Box<dyn chilli_pdf::pdf::PdfDocument<'s> + 's>, chilli_pdf::pdf::PdfError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.inner.open_document(path)
    }
}

fn converter_with(engine: CountingEngine, ocr: &OcrFactory) -> Arc<PipelineConverter> {
    let pdf_factory = {
        let engine = engine.clone();
        Arc::new(move || Ok(Box::new(engine.clone()) as Box<dyn PdfEngine>))
    };
    Arc::new(PipelineConverter::new(
        pdf_factory,
        Arc::clone(ocr),
        Arc::new(LopdfOverlay),
        glyphless_font(),
    ))
}

fn job(input: PathBuf, output: PathBuf) -> Job {
    Job {
        input,
        output,
        language: LanguagePreset::English,
    }
}

/// Starts the converter, waits for it to finish, and drains all events.
fn run_to_end(converter: &Arc<PipelineConverter>, test_job: Job) -> Vec<WorkerEvent> {
    let (events, receiver) = std::sync::mpsc::channel();
    let mut worker = Arc::clone(converter).start(test_job, events);
    worker.join();
    let mut drained = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        drained.push(event);
    }
    drained
}

/// Failure may legitimately follow `Progress` events (e.g. the publish step
/// runs after every page reported progress), but there must be exactly one
/// terminal `Failed` and no `Done`/`Cancelled`.
fn assert_failed_with(events: &[WorkerEvent], needle: &str) {
    let terminal: Vec<&WorkerEvent> = events
        .iter()
        .filter(|event| !matches!(event, WorkerEvent::Progress { .. }))
        .collect();
    let [WorkerEvent::Failed(message)] = terminal[..] else {
        panic!("expected a single Failed event, got {events:?}");
    };
    assert!(
        message.contains(needle),
        "expected message to contain {needle:?}, got {message:?}"
    );
}

/// A 300 DPI US Letter OCR page (2550×3300) with one line per word,
/// matching what `MockEngine` renders for a letter page.
fn letter_page(words: &[&str]) -> OcrPage {
    let lines = words
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let bottom = 2800 - index as i32 * 180;
            let width = 120 + text.len() as i32 * 90;
            OcrLine {
                rect: Rect::new(300, bottom, 2400, bottom + 100),
                words: vec![OcrWord {
                    text: (*text).to_string(),
                    confidence: 95.0,
                    rect: Rect::new(300, bottom, 300 + width, bottom + 100),
                }],
            }
        })
        .collect();
    OcrPage {
        width: 2550,
        height: 3300,
        dpi: 300,
        lines,
    }
}

// --- lopdf output walkers (patterns shared with tests/overlay.rs) --------

fn page_dict(document: &Document, page: usize) -> Dictionary {
    let page_id = *document
        .get_pages()
        .values()
        .nth(page)
        .expect("page number exists");
    document
        .get_object(page_id)
        .expect("page object")
        .as_dict()
        .expect("page dictionary")
        .clone()
}

fn contents_streams(document: &Document, page_dict: &Dictionary) -> Vec<Vec<u8>> {
    let contents = page_dict.get(b"Contents").expect("contents");
    match deref(document, contents) {
        Object::Stream(stream) => vec![stream.content.clone()],
        Object::Array(entries) => entries
            .iter()
            .map(|entry| {
                deref(document, entry)
                    .as_stream()
                    .expect("content stream")
                    .content
                    .clone()
            })
            .collect(),
        other => panic!("unexpected /Contents: {other:?}"),
    }
}

fn image_streams(document: &Document, page_dict: &Dictionary) -> Vec<Vec<u8>> {
    let resources = deref(document, page_dict.get(b"Resources").expect("resources"));
    let xobjects = match resources.as_dict().expect("resources dict").get(b"XObject") {
        Ok(object) => deref(document, object).as_dict().expect("xobjects dict"),
        Err(_) => return Vec::new(),
    };
    xobjects
        .iter()
        .map(|(_, object)| {
            deref(document, object)
                .as_stream()
                .expect("image xobject")
                .content
                .clone()
        })
        .collect()
}

fn deref<'a>(document: &'a Document, object: &'a Object) -> &'a Object {
    document.dereference(object).expect("dereference").1
}

/// Reads a `/MediaBox`-style entry as four numbers, tolerating the
/// Integer/Real distinction: lopdf's writer normalises `612.0` to `612`, so
/// an exactly-preserved box still changes `Object` variants on save + reload.
fn box_numbers(page_dict: &Dictionary, key: &[u8]) -> Option<[f64; 4]> {
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

/// Extracts the overlay text the pipeline appended to a page, by decoding
/// the UTF-16BE hex strings in the overlay content stream (the stream that
/// references the `ChilliOcrF1` font resource).
fn overlay_text(document: &Document, page: usize) -> String {
    let dict = page_dict(document, page);
    let streams = contents_streams(document, &dict);
    let overlay = streams
        .iter()
        .find(|bytes| String::from_utf8_lossy(bytes).contains("/ChilliOcrF1"))
        .expect("page has an overlay content stream");
    let text = String::from_utf8_lossy(overlay).into_owned();
    let mut words = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            break;
        };
        let hex = &after[..end];
        if hex.len().is_multiple_of(4) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let mut decoded = String::new();
            for pair in hex.as_bytes().chunks(4) {
                let unit =
                    u16::from_str_radix(std::str::from_utf8(pair).expect("hex is ascii"), 16)
                        .expect("hex unit");
                decoded.push(char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}'));
            }
            // Each word carries a single trailing space as padding.
            let decoded = decoded.trim_end();
            if !decoded.is_empty() {
                words.push(decoded.to_string());
            }
        }
        rest = &after[end + 1..];
    }
    words.join(" ")
}

fn temp_residue(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| is_temp_file_name(path.file_name().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------

fn letter_spec() -> MockPageSpec {
    MockPageSpec {
        crop_box: PageBox {
            left: 0.0,
            bottom: 0.0,
            right: 612.0,
            top: 792.0,
        },
        rotation: PageRotation::None,
        text: String::new(),
    }
}

#[test]
fn converts_a_scanned_english_pdf_end_to_end() {
    let scratch = scratch("eng");
    let input = fixture("gen_eng.pdf");
    let output = scratch.join("out.pdf");
    fs::copy(&input, scratch.join("gen_eng.pdf")).expect("copy input");
    let input = scratch.join("gen_eng.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, calls) = scripted_ocr(letter_page(&[
        "The quick brown fox",
        "jumps over the lazy dog",
    ]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input.clone(), output.clone()));
    assert_eq!(
        events,
        vec![
            WorkerEvent::Progress {
                page: 1,
                total_pages: 1
            },
            WorkerEvent::Done,
        ]
    );

    // The input is untouched, the output exists, and nothing leaked.
    assert_eq!(
        fs::read(&input).expect("input readable"),
        fs::read(fixture("gen_eng.pdf")).expect("fixture readable")
    );
    assert!(output.exists());
    assert!(temp_residue(&scratch).is_empty());
    assert_eq!(engine.opens.load(Ordering::SeqCst), 1);

    // OCR saw exactly one letter-sized 300 DPI render.
    assert_eq!(*calls.lock().expect("calls lock"), vec!["2550x3300@300"]);

    // The output is a loadable single-page PDF whose original content and
    // image survived the merge byte-for-byte.
    let before = Document::load(&input).expect("load input");
    let after = Document::load(&output).expect("load output");
    assert_eq!(after.get_pages().len(), 1);

    let before_dict = page_dict(&before, 0);
    let after_dict = page_dict(&after, 0);
    let before_contents = contents_streams(&before, &before_dict);
    let after_contents = contents_streams(&after, &after_dict);
    assert_eq!(before_contents.len(), 1);
    assert_eq!(after_contents.len(), 2, "overlay appended");
    assert_eq!(
        before_contents[0], after_contents[0],
        "original content stream unchanged"
    );
    assert_eq!(
        image_streams(&before, &before_dict),
        image_streams(&after, &after_dict),
        "image bytes unchanged"
    );

    // The invisible overlay carries the OCR words.
    let text = overlay_text(&after, 0);
    assert!(text.contains("The quick brown fox"), "got {text:?}");
    assert!(text.contains("jumps over the lazy dog"), "got {text:?}");
}

#[test]
fn round_trips_hungarian_diacritics_through_the_overlay() {
    let scratch = scratch("hun");
    let input = scratch.join("gen_hun.pdf");
    fs::copy(fixture("gen_hun.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, calls) = scripted_ocr(letter_page(&[
        "Árvíztűrő",
        "tükörfúrógép",
        "őrhajó",
        "űrséta",
    ]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input, output.clone()));
    assert_eq!(
        events,
        vec![
            WorkerEvent::Progress {
                page: 1,
                total_pages: 1
            },
            WorkerEvent::Done,
        ]
    );
    assert_eq!(calls.lock().expect("calls lock").len(), 1);

    let after = Document::load(&output).expect("load output");
    let text = overlay_text(&after, 0);
    for expected in ["Árvíztűrő", "tükörfúrógép", "őrhajó", "űrséta"] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    assert!(text.contains('ő'));
    assert!(text.contains('ű'));
}

#[test]
fn mixed_document_overlays_only_the_scanned_page() {
    let scratch = scratch("hybrid");
    let input = scratch.join("gen_hybrid.pdf");
    fs::copy(fixture("gen_hybrid.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");

    // Page 0 carries a real text layer and must be passed through; page 1
    // is a scan that needs the overlay. This is the exact shape that used
    // to break the positional overlay merge (see issue #18).
    let searchable = MockPageSpec::searchable();
    let scanned = letter_spec();
    let engine = CountingEngine {
        inner: MockEngine::with_spec(vec![searchable, scanned]),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, calls) = scripted_ocr(letter_page(&["overlay", "only", "here"]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input.clone(), output.clone()));
    assert_eq!(
        events,
        vec![
            WorkerEvent::Progress {
                page: 1,
                total_pages: 2
            },
            WorkerEvent::Progress {
                page: 2,
                total_pages: 2
            },
            WorkerEvent::Done,
        ]
    );

    // Only the scanned page reached the recognizer.
    assert_eq!(calls.lock().expect("calls lock").len(), 1);

    let before = Document::load(&input).expect("load input");
    let after = Document::load(&output).expect("load output");
    assert_eq!(after.get_pages().len(), 2);

    // Text page: untouched, no overlay appended.
    let before_text_dict = page_dict(&before, 0);
    let after_text_dict = page_dict(&after, 0);
    let before_text_streams = contents_streams(&before, &before_text_dict);
    let after_text_streams = contents_streams(&after, &after_text_dict);
    assert_eq!(before_text_streams.len(), 1);
    assert_eq!(after_text_streams, before_text_streams);
    assert!(String::from_utf8_lossy(&after_text_streams[0]).contains("BT"));
    assert!(image_streams(&after, &after_text_dict).is_empty());

    // Scanned page: overlay appended, image preserved.
    let before_scan_dict = page_dict(&before, 1);
    let after_scan_dict = page_dict(&after, 1);
    let before_scan_streams = contents_streams(&before, &before_scan_dict);
    let after_scan_streams = contents_streams(&after, &after_scan_dict);
    assert_eq!(before_scan_streams.len(), 1);
    assert_eq!(after_scan_streams.len(), 2);
    assert_eq!(before_scan_streams[0], after_scan_streams[0]);
    assert_eq!(
        image_streams(&before, &before_scan_dict),
        image_streams(&after, &after_scan_dict)
    );
    let text = overlay_text(&after, 1);
    assert!(text.contains("overlay"), "got {text:?}");
}

#[test]
fn converts_a_rotated_scan_and_preserves_rotation_and_geometry() {
    let scratch = scratch("rotated");
    let input = scratch.join("gen_rotated.pdf");
    fs::copy(fixture("gen_rotated.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");

    let spec = MockPageSpec {
        crop_box: PageBox {
            left: 0.0,
            bottom: 0.0,
            right: 792.0,
            top: 612.0,
        },
        rotation: PageRotation::Degrees90,
        text: String::new(),
    };
    let engine = CountingEngine {
        inner: MockEngine::with_spec(vec![spec]),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, calls) = scripted_ocr(letter_page(&["rotated", "scan"]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input.clone(), output.clone()));
    assert_eq!(
        events,
        vec![
            WorkerEvent::Progress {
                page: 1,
                total_pages: 1
            },
            WorkerEvent::Done,
        ]
    );
    // A sideways letter renders to the same 2550×3300 pixels.
    assert_eq!(*calls.lock().expect("calls lock"), vec!["2550x3300@300"]);

    let before = Document::load(&input).expect("load input");
    let after = Document::load(&output).expect("load output");
    let before_dict = page_dict(&before, 0);
    let after_dict = page_dict(&after, 0);
    let rotate = after_dict
        .get(b"Rotate")
        .ok()
        .and_then(|object| object.as_i64().ok());
    assert_eq!(rotate, Some(90), "/Rotate preserved");
    let before_box = box_numbers(&before_dict, b"MediaBox").expect("mediabox");
    let after_box = box_numbers(&after_dict, b"MediaBox").expect("mediabox");
    assert_eq!(before_box, after_box);
    assert_eq!(
        image_streams(&before, &before_dict),
        image_streams(&after, &after_dict)
    );
    let text = overlay_text(&after, 0);
    assert!(text.contains("rotated"), "got {text:?}");
}

#[test]
fn converts_a_multi_page_scan_page_by_page() {
    let scratch = scratch("multi");
    let input = scratch.join("gen_multi.pdf");
    fs::copy(fixture("gen_multi.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(5),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, calls) = scripted_ocr(letter_page(&["page", "words"]));
    let converter = converter_with(engine.clone(), &ocr);

    let mut expected = (1..=5)
        .map(|page| WorkerEvent::Progress {
            page,
            total_pages: 5,
        })
        .collect::<Vec<_>>();
    expected.push(WorkerEvent::Done);
    let events = run_to_end(&converter, job(input.clone(), output.clone()));
    assert_eq!(events, expected);
    assert_eq!(calls.lock().expect("calls lock").len(), 5);

    let before = Document::load(&input).expect("load input");
    let after = Document::load(&output).expect("load output");
    assert_eq!(after.get_pages().len(), 5);
    for page in 0..5 {
        let before_dict = page_dict(&before, page);
        let after_dict = page_dict(&after, page);
        assert_eq!(
            contents_streams(&after, &after_dict).len(),
            2,
            "page {page} has an overlay"
        );
        assert_eq!(
            image_streams(&before, &before_dict),
            image_streams(&after, &after_dict),
            "page {page} image unchanged"
        );
        assert!(overlay_text(&after, page).contains("page"));
    }
}

#[test]
fn handles_unicode_and_spaces_in_paths() {
    let scratch = scratch("unicode");
    let folder = scratch.join("Űrlap mappa");
    fs::create_dir_all(&folder).expect("create unicode folder");
    let input = folder.join("Gőzgép készített szűrőt.pdf");
    fs::copy(fixture("gen_hun.pdf"), &input).expect("copy input");
    let output = default_output_path(&input);

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["űrlap"]));
    let converter = converter_with(engine, &ocr);

    let events = run_to_end(&converter, job(input.clone(), output.clone()));
    assert_eq!(
        events,
        vec![
            WorkerEvent::Progress {
                page: 1,
                total_pages: 1
            },
            WorkerEvent::Done,
        ]
    );
    assert!(output.exists(), "default output path used");
    assert_eq!(
        output.file_name().and_then(std::ffi::OsStr::to_str),
        Some("Gőzgép készített szűrőt_searchable.pdf")
    );
    let after = Document::load(&output).expect("load output");
    assert!(overlay_text(&after, 0).contains("űrlap"));
}

// ---------------------------------------------------------------------------
// Negative paths
// ---------------------------------------------------------------------------

#[test]
fn rejects_an_encrypted_pdf_with_a_typed_error() {
    let scratch = scratch("encrypted");
    let input = scratch.join("encrypted_scan.pdf");
    fs::copy(fixture("encrypted_scan.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["never", "reached"]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input.clone(), output.clone()));
    assert_failed_with(&events, "password-protected");
    assert!(!output.exists());
    assert!(temp_residue(&scratch).is_empty());
    assert_eq!(
        engine.opens.load(Ordering::SeqCst),
        0,
        "validation fires before the engine opens anything"
    );
    assert_eq!(
        fs::read(&input).expect("input readable"),
        fs::read(fixture("encrypted_scan.pdf")).expect("fixture readable"),
        "input untouched"
    );
}

#[test]
fn rejects_malformed_and_truncated_files_without_panicking() {
    let scratch = scratch("malformed");

    // Deterministic pseudo-random garbage (no RNG dependency).
    let mut garbage = vec![0u8; 4096];
    for (index, byte) in garbage.iter_mut().enumerate() {
        *byte = (index.wrapping_mul(31) % 251) as u8;
    }

    let truncated = {
        let bytes = fs::read(fixture("gen_eng.pdf")).expect("fixture");
        bytes[..bytes.len() / 2].to_vec()
    };

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("plain-text", b"this is definitely not a pdf".to_vec()),
        ("cut-header", b"%PDF-1.7 cut off".to_vec()),
        ("garbage", garbage),
        ("truncated-fixture", truncated),
    ];

    for (label, bytes) in cases {
        let input = scratch.join(format!("{label}.pdf"));
        let output = scratch.join(format!("{label}-out.pdf"));
        fs::write(&input, &bytes).expect("write input");

        let engine = CountingEngine {
            inner: MockEngine::with_pages(1),
            opens: Arc::new(AtomicUsize::new(0)),
        };
        let (ocr, _) = scripted_ocr(letter_page(&["never", "reached"]));
        let converter = converter_with(engine.clone(), &ocr);

        let events = run_to_end(&converter, job(input.clone(), output.clone()));
        assert_failed_with(&events, "does not look like a valid PDF");
        assert!(!output.exists(), "{label}: no output");
        assert!(temp_residue(&scratch).is_empty(), "{label}: no temp files");
        assert_eq!(
            engine.opens.load(Ordering::SeqCst),
            0,
            "{label}: never opened"
        );
    }
}

#[test]
fn never_overwrites_the_input_file() {
    let scratch = scratch("same-as-input");
    let input = scratch.join("gen_eng.pdf");
    fs::copy(fixture("gen_eng.pdf"), &input).expect("copy input");
    let before = fs::read(&input).expect("read input");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["never", "reached"]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input.clone(), input.clone()));
    assert_failed_with(&events, "never overwrites the original");
    assert_eq!(fs::read(&input).expect("read input"), before);
    assert_eq!(engine.opens.load(Ordering::SeqCst), 0);
}

#[test]
fn rejects_an_oversized_pdf_before_reading_it() {
    let scratch = scratch("oversized");
    let input = scratch.join("huge.pdf");
    // A sparse file costs nothing on any platform, and validate_size runs
    // before the pipeline reads the bytes.
    fs::File::create(&input)
        .and_then(|file| file.set_len(DEFAULT_MAX_SIZE_BYTES + 1))
        .expect("create sparse file");
    let output = scratch.join("out.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["never", "reached"]));
    let converter = converter_with(engine.clone(), &ocr);

    let events = run_to_end(&converter, job(input, output.clone()));
    assert_failed_with(&events, "larger than the supported limit");
    assert!(!output.exists());
    assert!(temp_residue(&scratch).is_empty());
    assert_eq!(engine.opens.load(Ordering::SeqCst), 0);
}

#[test]
fn cooperative_cancel_mid_document_leaves_no_trace() {
    let scratch = scratch("cancel");
    let input = scratch.join("gen_multi.pdf");
    fs::copy(fixture("gen_multi.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(300),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = delayed_ocr(letter_page(&["slow", "page"]), Duration::from_millis(3));
    let converter = converter_with(engine, &ocr);

    let (events, receiver) = std::sync::mpsc::channel();
    let mut worker = Arc::clone(&converter).start(job(input, output.clone()), events);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let Ok(event) = receiver.recv_timeout(Duration::from_millis(20)) else {
            assert!(Instant::now() < deadline, "timed out waiting for progress");
            continue;
        };
        if matches!(event, WorkerEvent::Progress { page: 1, .. }) {
            worker.cancel();
            break;
        }
        if matches!(event, WorkerEvent::Done | WorkerEvent::Failed(_)) {
            panic!("converter finished before we could cancel: {event:?}");
        }
    }
    worker.join();

    let mut collected = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        collected.push(event);
    }
    assert!(
        collected.contains(&WorkerEvent::Cancelled),
        "expected a Cancelled event, got {collected:?}"
    );
    assert!(!collected.contains(&WorkerEvent::Done));
    assert!(
        !collected
            .iter()
            .any(|event| matches!(event, WorkerEvent::Failed(_))),
        "cancel must not be reported as a failure"
    );
    assert!(!output.exists(), "cancelled runs must not publish");
    assert!(temp_residue(&scratch).is_empty(), "no temp residue");
}

// ---------------------------------------------------------------------------
// Output destination edge cases
// ---------------------------------------------------------------------------

#[test]
fn missing_output_directory_fails_with_a_typed_error() {
    let scratch = scratch("missing-dir");
    let input = scratch.join("gen_eng.pdf");
    fs::copy(fixture("gen_eng.pdf"), &input).expect("copy input");
    let output = scratch.join("no").join("such").join("dir").join("out.pdf");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["validated", "but unpublishable"]));
    let converter = converter_with(engine, &ocr);

    let events = run_to_end(&converter, job(input, output.clone()));
    assert_failed_with(&events, "could not be written to");
    assert!(!output.exists());
}

#[test]
#[cfg(unix)]
fn read_only_output_directory_fails_without_panicking() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = scratch("read-only");
    let input = scratch.join("gen_eng.pdf");
    fs::copy(fixture("gen_eng.pdf"), &input).expect("copy input");
    let dir = scratch.join("locked");
    fs::create_dir_all(&dir).expect("create output dir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");

    // If the current user can still write (e.g. running as root), the
    // assertion would be meaningless — restore and skip.
    let probe = dir.join(".probe");
    let writable_probe = fs::write(&probe, b"x").is_ok();
    let _ = fs::remove_file(&probe);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("restore perms");
    if writable_probe {
        eprintln!("skipping: output directory is writable for this user");
        return;
    }

    let output = dir.join("out.pdf");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["validated", "but unpublishable"]));
    let converter = converter_with(engine, &ocr);

    let events = run_to_end(&converter, job(input, output.clone()));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("restore perms");

    assert_failed_with(&events, "could not be written to");
    assert!(!output.exists());
    assert!(temp_residue(&dir).is_empty());
}

#[test]
fn output_conflicts_are_reported_by_check_output() {
    let scratch = scratch("conflicts");
    let input = scratch.join("gen_eng.pdf");
    fs::copy(fixture("gen_eng.pdf"), &input).expect("copy input");

    let same = check_output(&input, &input);
    assert_eq!(
        same,
        Err(Conflict::SameAsInput),
        "same-as-input is always fatal"
    );

    let existing = scratch.join("out.pdf");
    fs::write(&existing, b"old bytes").expect("write existing");
    let other = fixture("gen_hun.pdf");
    assert_eq!(
        check_output(&input, &existing),
        Err(Conflict::AlreadyExists(existing.clone()))
    );
    assert_eq!(
        check_output(&other, &existing),
        Err(Conflict::AlreadyExists(existing.clone()))
    );
}

#[test]
fn pipeline_replaces_an_existing_output_transactionally() {
    let scratch = scratch("replace");
    let input = scratch.join("gen_eng.pdf");
    fs::copy(fixture("gen_eng.pdf"), &input).expect("copy input");
    let output = scratch.join("out.pdf");
    fs::write(&output, b"stale output from an earlier run").expect("write stale output");

    let engine = CountingEngine {
        inner: MockEngine::with_pages(1),
        opens: Arc::new(AtomicUsize::new(0)),
    };
    let (ocr, _) = scripted_ocr(letter_page(&["replacement", "wins"]));
    let converter = converter_with(engine, &ocr);

    let events = run_to_end(&converter, job(input, output.clone()));
    assert!(
        matches!(events.last(), Some(WorkerEvent::Done)),
        "got {events:?}"
    );

    let bytes = fs::read(&output).expect("read output");
    assert!(bytes.starts_with(b"%PDF"), "stale bytes were replaced");
    let after = Document::load_mem(&bytes).expect("load output");
    assert!(overlay_text(&after, 0).contains("replacement"));
    assert!(temp_residue(&scratch).is_empty());
}

// ---------------------------------------------------------------------------
// Validation config contract
// ---------------------------------------------------------------------------

#[test]
fn validation_rejects_oversized_bytes_for_a_tighter_limit() {
    let config = ValidationConfig::with_max_size_bytes(64);
    let bytes = vec![b'%'; 1024];
    let error = validate_bytes(&bytes, &config).expect_err("must reject");
    assert!(
        error
            .user_message()
            .contains("larger than the supported limit")
    );
}
