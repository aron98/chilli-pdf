//! End-to-end conversion driver for the CI pipeline job (issue #42).
//!
//! Runs the PRODUCTION conversion pipeline — real bundled PDFium and
//! Tesseract wired exactly like the shipped app (`PipelineConverter::
//! production()`, per-user extracted assets, no scratch/system fallback,
//! no mocks) — over three committed fixtures and asserts, in Rust:
//!
//! * `eng_scan.pdf` + English: the overlay carries the ground-truth
//!   words and no replacement characters;
//! * `hun_scan.pdf` + Hungarian: the overlay carries precomposed
//!   `ő`/`ű`, a ground-truth accented word, and no mojibake
//!   (U+FFFD, U+0308, U+030B);
//! * `rotated_scan.pdf` + English: `/Rotate 90` survives, the boxes stay
//!   put, the original content streams stay byte-identical, and the
//!   overlay holds real words;
//!
//! plus the exact happy-path event stream per conversion, structural
//! output verification with lopdf (the `tests/overlay.rs` style walkers),
//! fixture integrity before/after, and no `.chilli-pdf-tmp` residue.
//!
//! Usage:
//!     cargo run --example e2e_convert -- [--out <dir>]
//!
//! This is the lean CI counterpart of `examples/verify_acceptance.rs`
//! (the exhaustive dev-time driver). It deliberately refuses to run in
//! fixture-bundle builds (missing `--cfg chilli_real_assets`) so a green
//! run always means real engines ran; the refusal is a runtime check,
//! so the driver still compiles (and clippy-checks) in both modes.
//!
//! Exit codes: 0 all checks passed, 1 at least one failed, 2 the build
//! uses the fixture asset bundle (real assets required).

use chilli_pdf::converter::{Converter, Job, WorkerEvent};
use chilli_pdf::language::LanguagePreset;
use chilli_pdf::transactional::is_temp_file_name;
use chilli_pdf::worker::PipelineConverter;
use lopdf::{Dictionary, Document, Object};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

const USAGE: &str = "usage: cargo run --example e2e_convert -- [--out <dir>]";

/// The three fixtures this driver converts. Each is SHA-256-checked
/// before and after everything: the pipeline must never modify inputs.
const FIXTURE_PDFS: [&str; 3] = ["eng_scan.pdf", "hun_scan.pdf", "rotated_scan.pdf"];

fn main() {
    let started = Instant::now();
    let out_dir = parse_args();

    println!(
        "chilli-pdf {} e2e conversion driver (issue #42 CI job)",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "  bundle: {}",
        if cfg!(chilli_real_assets) {
            "real assets (--cfg chilli_real_assets)"
        } else {
            "fixture placeholders"
        }
    );
    println!("  output: {}", out_dir.display());

    if !cfg!(chilli_real_assets) {
        eprintln!();
        eprintln!("ERROR: this build uses the fixture asset bundle, so OCR would run");
        eprintln!("against placeholder bytes and prove nothing. Fetch the real bundle");
        eprintln!("first (build-time network), then rebuild and re-run:");
        eprintln!();
        eprintln!("    scripts/fetch-assets");
        eprintln!("    cargo run --example e2e_convert -- [--out <dir>]");
        exit(2);
    }

    if out_dir.exists() {
        fs::remove_dir_all(&out_dir)
            .unwrap_or_else(|e| die(&format!("cannot clean {}: {e}", out_dir.display())));
    }
    fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| die(&format!("cannot create {}: {e}", out_dir.display())));

    let before_digests = fixture_digests().unwrap_or_else(|e| die(&e));

    let mut report = Report::default();
    let ctx = Ctx {
        converter: Arc::new(PipelineConverter::production()),
        out: out_dir.clone(),
    };
    report.record("convert: eng_scan + English", check_eng(&ctx));
    report.record("convert: hun_scan + Hungarian (ő/ű)", check_hun(&ctx));
    report.record(
        "convert: rotated_scan keeps /Rotate 90",
        check_rotated(&ctx),
    );
    report.record(
        "integrity: fixtures unchanged",
        check_fixture_integrity(&before_digests),
    );
    report.record(
        "integrity: no temp residue",
        check_no_temp_residue(&out_dir),
    );

    let total = report.passed + report.failed.len();
    println!();
    println!(
        "{total} checks in {:.1}s: {} passed, {} failed",
        started.elapsed().as_secs_f64(),
        report.passed,
        report.failed.len()
    );
    for name in &report.failed {
        println!("  FAILED: {name}");
    }
    exit(i32::from(!report.failed.is_empty()));
}

/// Parse `[--out <dir>]`; the default scratch tree lives under `target/`.
fn parse_args() -> PathBuf {
    let mut args = env::args_os().skip(1);
    let mut out: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--help" | "-h" => {
                println!("{USAGE}");
                exit(0);
            }
            "--out" => {
                out = Some(args.next().map(PathBuf::from).unwrap_or_else(|| die(USAGE)));
            }
            other => die(&format!("unknown argument {other:?} — {USAGE}")),
        }
    }
    out.unwrap_or_else(|| repo_path("target/e2e-convert"))
}

/// Absolute path to `relative` inside the crate, wherever cargo runs us.
fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn fixture_path(name: &str) -> PathBuf {
    repo_path("tests/fixtures").join(name)
}

fn die(message: &str) -> ! {
    eprintln!("e2e_convert: {message}");
    exit(1);
}

/// Per-check bookkeeping: one PASS with evidence lines or one FAIL with
/// the reason.
#[derive(Default)]
struct Report {
    passed: usize,
    failed: Vec<String>,
}

impl Report {
    fn record(&mut self, name: &str, outcome: Result<Vec<String>, String>) {
        println!();
        match outcome {
            Ok(evidence) => {
                self.passed += 1;
                println!("[PASS] {name}");
                for line in evidence {
                    println!("       {line}");
                }
            }
            Err(reason) => {
                self.failed.push(name.to_owned());
                println!("[FAIL] {name}");
                println!("       {reason}");
            }
        }
    }
}

/// One production converter shared by every check (the real engines live
/// on its actor thread) plus the scratch root.
struct Ctx {
    converter: Arc<PipelineConverter>,
    out: PathBuf,
}

// --- conversions -----------------------------------------------------------

/// One finished conversion: paths, event stream, structural facts.
struct Conversion {
    input: PathBuf,
    output: PathBuf,
    events: Vec<WorkerEvent>,
    structure: Structure,
}

impl Conversion {
    /// The decoded overlay text of `page` (0-based); fails the check when
    /// the page was left without an overlay.
    fn overlay(&self, page: usize) -> Result<&str, String> {
        self.structure
            .overlays
            .get(page)
            .and_then(|text| text.as_deref())
            .ok_or_else(|| format!("page {} carries no overlay text", page + 1))
    }

    fn evidence_lines(&self) -> Vec<String> {
        let size = fs::metadata(&self.output)
            .map(|meta| meta.len())
            .unwrap_or(0);
        vec![
            format!("input:  {}", self.input.display()),
            format!("output: {} ({size} bytes)", self.output.display()),
            format!("events: {:?}", self.events),
            format!("pages/rotation: {:?}", self.structure.rotates),
            format!("MediaBox: {:?}", self.structure.media_boxes),
            format!("CropBox: {:?}", self.structure.crop_boxes),
            format!(
                "content streams per page, before → after: {:?}",
                self.structure.stream_counts
            ),
        ]
    }
}

/// Run one fixture through the production pipeline into its own scratch
/// directory under the conventional `<stem>_searchable.pdf` name, then
/// assert the structural invariants.
fn convert_fixture(
    ctx: &Ctx,
    dir_name: &str,
    fixture: &str,
    language: LanguagePreset,
) -> Result<Conversion, String> {
    let dir = ctx.out.join(dir_name);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let input = dir.join(fixture);
    fs::copy(fixture_path(fixture), &input)
        .map_err(|e| format!("cannot copy fixture {fixture}: {e}"))?;
    let output = dir.join(format!(
        "{}_searchable.pdf",
        fixture.strip_suffix(".pdf").unwrap_or(fixture)
    ));

    // Exact happy-path event stream: one Progress per page, then Done.
    let (events, inbox) = mpsc::channel();
    let mut worker = Arc::clone(&ctx.converter).start(
        Job {
            input: input.clone(),
            output: output.clone(),
            language,
        },
        events,
    );
    worker.join();
    let events: Vec<WorkerEvent> = inbox.try_iter().collect();
    let expected = vec![
        WorkerEvent::Progress {
            page: 1,
            total_pages: 1,
        },
        WorkerEvent::Done,
    ];
    if events != expected {
        return Err(format!(
            "unexpected event stream {events:?} (wanted {expected:?})"
        ));
    }

    let structure = verify_structure(&input, &output)?;
    if structure.pages != 1 {
        return Err(format!("expected 1 page, found {}", structure.pages));
    }
    Ok(Conversion {
        input,
        output,
        events,
        structure,
    })
}

fn check_eng(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(ctx, "convert-eng", "eng_scan.pdf", LanguagePreset::English)?;
    let text = conv.overlay(0)?;
    require(
        text.contains("quick") && text.contains("fox"),
        format!("ground-truth words missing from the overlay: {text:?}"),
    )?;
    require(
        !text.contains('\u{FFFD}'),
        format!("overlay text contains U+FFFD: {text:?}"),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(format!("decoded overlay text: {text:?}"));
    Ok(evidence)
}

fn check_hun(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-hun",
        "hun_scan.pdf",
        LanguagePreset::Hungarian,
    )?;
    let text = conv.overlay(0)?;
    for needle in ['ő', 'ű'] {
        require(
            text.contains(needle),
            format!("overlay text lost {needle:?}: {text:?}"),
        )?;
    }
    let ground_truth = ["Árvíztűrő", "tükörfúrógép", "űrhajós", "Őssel"];
    require(
        ground_truth.iter().any(|word| text.contains(word)),
        format!("no ground-truth accented word survived: {text:?}"),
    )?;
    // No mojibake: no replacement char, no decomposed accents where
    // precomposed ő/ű belong.
    require(
        !text.contains('\u{FFFD}') && !text.contains('\u{0308}') && !text.contains('\u{030B}'),
        format!("mojibake in overlay text: {text:?}"),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(format!("decoded overlay text: {text:?}"));
    evidence.push("ő (U+0151) and ű (U+0171) present as single precomposed characters".to_owned());
    Ok(evidence)
}

fn check_rotated(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-rotated",
        "rotated_scan.pdf",
        LanguagePreset::English,
    )?;
    require(
        conv.structure.rotates[0] == Some(90),
        format!("/Rotate not preserved: {:?}", conv.structure.rotates),
    )?;
    let text = conv.overlay(0)?;
    let words = word_count(text);
    require(
        words >= 3,
        format!("only {words} overlay words on the rotated page: {text:?}"),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(format!(
        "overlay on the 90°-rotated page ({words} words): {text:?}"
    ));
    Ok(evidence)
}

fn word_count(text: &str) -> usize {
    text.split(' ').filter(|word| !word.is_empty()).count()
}

fn require(condition: bool, message: String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message) }
}

// --- integrity ------------------------------------------------------------

fn fixture_digests() -> Result<BTreeMap<String, String>, String> {
    let mut digests = BTreeMap::new();
    for name in FIXTURE_PDFS {
        let bytes =
            fs::read(fixture_path(name)).map_err(|e| format!("cannot read fixture {name}: {e}"))?;
        digests.insert(name.to_owned(), hex::encode(Sha256::digest(&bytes)));
    }
    Ok(digests)
}

fn check_fixture_integrity(before: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let after = fixture_digests()?;
    let mut evidence = Vec::new();
    for (name, digest) in &after {
        require(
            before.get(name) == Some(digest),
            format!(
                "fixture {name} changed during the run: {:?} → {digest:?}",
                before.get(name)
            ),
        )?;
        evidence.push(format!("sha256 {name}: {digest}"));
    }
    evidence.push(format!(
        "{} fixture files byte-identical before and after the run",
        after.len()
    ));
    Ok(evidence)
}

fn check_no_temp_residue(out_dir: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    collect_temp_residue(out_dir, &mut found);
    require(
        found.is_empty(),
        format!("temp files left behind: {found:?}"),
    )?;
    Ok(vec![format!(
        "no .chilli-pdf-tmp residue anywhere under {}",
        out_dir.display()
    )])
}

fn collect_temp_residue(dir: &Path, found: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_temp_residue(&path, found);
            }
        }
    }
    found.extend(temp_residue(dir));
}

fn temp_residue(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.file_name().is_some_and(is_temp_file_name))
                .collect()
        })
        .unwrap_or_default()
}

// --- lopdf output walkers (patterns shared with tests/overlay.rs) ---------

/// Structural facts of one converted document, with the invariants the
/// pipeline promises already asserted: equal page count; MediaBox,
/// CropBox and /Rotate unchanged; original content streams byte-equal
/// and never dropped; image streams untouched.
struct Structure {
    pages: usize,
    media_boxes: Vec<Option<[f64; 4]>>,
    crop_boxes: Vec<Option<[f64; 4]>>,
    rotates: Vec<Option<i64>>,
    /// Per page: content-stream count before → after the merge.
    stream_counts: Vec<(usize, usize)>,
    /// Per page: decoded overlay words, or `None` when untouched.
    overlays: Vec<Option<String>>,
}

fn verify_structure(input: &Path, output: &Path) -> Result<Structure, String> {
    let before = load_doc(input)?;
    let after = load_doc(output)?;
    if before.get_pages().len() != after.get_pages().len() {
        return Err(format!(
            "page count changed: {} → {}",
            before.get_pages().len(),
            after.get_pages().len()
        ));
    }
    let mut structure = Structure {
        pages: before.get_pages().len(),
        media_boxes: Vec::new(),
        crop_boxes: Vec::new(),
        rotates: Vec::new(),
        stream_counts: Vec::new(),
        overlays: Vec::new(),
    };
    for page in 0..structure.pages {
        let before_dict = page_dict(&before, page)?;
        let after_dict = page_dict(&after, page)?;

        require(
            box_numbers(&before_dict, b"MediaBox") == box_numbers(&after_dict, b"MediaBox"),
            format!(
                "page {}: MediaBox changed: {:?} → {:?}",
                page + 1,
                box_numbers(&before_dict, b"MediaBox"),
                box_numbers(&after_dict, b"MediaBox"),
            ),
        )?;
        structure
            .media_boxes
            .push(box_numbers(&after_dict, b"MediaBox"));

        require(
            box_numbers(&before_dict, b"CropBox") == box_numbers(&after_dict, b"CropBox"),
            format!(
                "page {}: CropBox changed: {:?} → {:?}",
                page + 1,
                box_numbers(&before_dict, b"CropBox"),
                box_numbers(&after_dict, b"CropBox"),
            ),
        )?;
        structure
            .crop_boxes
            .push(box_numbers(&after_dict, b"CropBox"));

        let before_rotate = rotate_value(&before_dict);
        let after_rotate = rotate_value(&after_dict);
        require(
            before_rotate == after_rotate,
            format!(
                "page {}: /Rotate changed: {before_rotate:?} → {after_rotate:?}",
                page + 1
            ),
        )?;
        structure.rotates.push(after_rotate);

        let before_streams = contents_streams(&before, &before_dict)?;
        let after_streams = contents_streams(&after, &after_dict)?;
        require(
            after_streams.len() >= before_streams.len(),
            format!(
                "page {}: content streams shrank: {} → {}",
                page + 1,
                before_streams.len(),
                after_streams.len()
            ),
        )?;
        for (index, bytes) in before_streams.iter().enumerate() {
            require(
                &after_streams[index] == bytes,
                format!(
                    "page {}: original content stream {index} was modified",
                    page + 1
                ),
            )?;
        }
        structure
            .stream_counts
            .push((before_streams.len(), after_streams.len()));

        require(
            image_streams(&before, &before_dict)? == image_streams(&after, &after_dict)?,
            format!("page {}: image streams changed", page + 1),
        )?;

        structure.overlays.push(overlay_text(&after, page)?);
    }
    Ok(structure)
}

fn load_doc(path: &Path) -> Result<Document, String> {
    Document::load(path).map_err(|e| format!("cannot load {}: {e:?}", path.display()))
}

fn deref<'a>(document: &'a Document, object: &'a Object) -> Result<&'a Object, String> {
    document
        .dereference(object)
        .map(|(_, resolved)| resolved)
        .map_err(|_| "dangling reference".to_owned())
}

fn page_dict(document: &Document, page: usize) -> Result<Dictionary, String> {
    let page_id = *document
        .get_pages()
        .values()
        .nth(page)
        .ok_or_else(|| format!("page {} missing", page + 1))?;
    document
        .get_object(page_id)
        .map_err(|_| format!("page {} object unreadable", page + 1))?
        .as_dict()
        .cloned()
        .map_err(|_| format!("page {} is not a dictionary", page + 1))
}

fn contents_streams(document: &Document, page_dict: &Dictionary) -> Result<Vec<Vec<u8>>, String> {
    let contents = page_dict
        .get(b"Contents")
        .map_err(|_| "page has no /Contents".to_owned())?;
    match deref(document, contents)? {
        Object::Stream(stream) => Ok(vec![stream.content.clone()]),
        Object::Array(entries) => {
            let mut streams = Vec::new();
            for entry in entries {
                streams.push(
                    deref(document, entry)?
                        .as_stream()
                        .map_err(|_| "content entry is not a stream".to_owned())?
                        .content
                        .clone(),
                );
            }
            Ok(streams)
        }
        other => Err(format!("unexpected /Contents: {other:?}")),
    }
}

fn image_streams(document: &Document, page_dict: &Dictionary) -> Result<Vec<Vec<u8>>, String> {
    let resources = page_dict
        .get(b"Resources")
        .map_err(|_| "page has no /Resources".to_owned())?;
    let resources = deref(document, resources)?
        .as_dict()
        .map_err(|_| "resources is not a dictionary".to_owned())?;
    let Ok(xobjects) = resources.get(b"XObject") else {
        return Ok(Vec::new());
    };
    let xobjects = deref(document, xobjects)?
        .as_dict()
        .map_err(|_| "xobjects is not a dictionary".to_owned())?;
    let mut streams = Vec::new();
    for (_, object) in xobjects.iter() {
        streams.push(
            deref(document, object)?
                .as_stream()
                .map_err(|_| "xobject is not a stream".to_owned())?
                .content
                .clone(),
        );
    }
    Ok(streams)
}

/// Reads a `/MediaBox`-style entry as four numbers, tolerating the
/// Integer/Real distinction (mirrors `tests/integration.rs`).
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

fn rotate_value(page_dict: &Dictionary) -> Option<i64> {
    page_dict
        .get(b"Rotate")
        .ok()
        .and_then(|object| object.as_i64().ok())
}

/// The invisible overlay words appended to `page`, decoded from the
/// UTF-16BE hex strings of the overlay content stream (the stream that
/// references the `ChilliOcrF1` font resource); `None` when the page
/// was left untouched (mirrors `tests/integration.rs`).
fn overlay_text(document: &Document, page: usize) -> Result<Option<String>, String> {
    let dict = page_dict(document, page)?;
    let streams = contents_streams(document, &dict)?;
    let Some(overlay) = streams
        .iter()
        .find(|bytes| String::from_utf8_lossy(bytes).contains("ChilliOcrF1"))
    else {
        return Ok(None);
    };
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
                words.push(decoded.to_owned());
            }
        }
        rest = &after[end + 1..];
    }
    Ok(Some(words.join(" ")))
}
