//! Acceptance verification driver for issue #21: run the PRODUCTION
//! conversion pipeline — the real bundled PDFium and Tesseract, wired
//! exactly like the shipped app (`PipelineConverter::production`) — over
//! the committed fixtures, and print one evidence block per acceptance
//! check of `docs/verification-plan.md`.
//!
//! Usage:
//!     cargo run --example verify_acceptance -- [--bundled-assets] [--out <dir>]
//!
//! Why an example and not a test: the run needs the real asset bundle
//! (fetch it with `scripts/fetch-assets` first) and spends seconds of
//! real OCR per page. `cargo test` stays fast, offline and mock-driven;
//! this driver is the dev-time counterpart that proves the pipeline with
//! real engines. It refuses to run in fixture-bundle builds (missing
//! `--cfg chilli_real_assets`) so a green run always means real engines
//! ran — the refusal is a runtime check, so the whole driver still
//! compiles (and clippy-checks) in both bundle modes.
//!
//! What it checks (mapped one-to-one in `docs/verification-report.md`):
//! * conversions of the English, Hungarian and mixed-language scans,
//!   the rotated and cropped scans, the multi-page and hybrid documents,
//!   and a Unicode-named input — with structural assertions on page
//!   count, boxes, rotation, untouched original content streams and the
//!   decoded invisible-text overlay (Hungarian `ő`/`ű` must survive
//!   exactly, with no U+FFFD and no decomposed accents);
//! * rejections (encrypted, signed, malformed, output==input) with the
//!   exact user-facing messages, no output and no temp residue;
//! * the output-collision contract (ask before replacing, then replace
//!   transactionally) and cooperative cancellation leaving no trace;
//! * fixture integrity: every input file byte-identical after the run,
//!   and no `.chilli-pdf-tmp` residue anywhere in the scratch tree.
//!
//! Evidence that needs external tools (pdftotext/pypdf extraction, viewer
//! search/copy, 150 DPI raster comparison, network observation) is
//! gathered around this driver and recorded in the verification report.
//!
//! `--bundled-assets` is the (default and only) acceptance mode: the run
//! must get every engine and model from the per-user extracted bundle
//! (`PipelineConverter::production()` never consults a scratch or
//! system fallback, and the bundled-assets check proves it — including
//! refusing the historical placeholder bytes). The flag exists so CI
//! and dev scripts can state that contract explicitly.
//!
//! Exit codes: 0 all checks passed, 1 at least one failed, 2 the build
//! uses the fixture asset bundle (real assets required).

use chilli_pdf::assets::{AssetManifest, NativeLib, ensure_assets, manifest};
use chilli_pdf::converter::{Converter, Job, WorkerEvent};
use chilli_pdf::language::LanguagePreset;
use chilli_pdf::output_path::{Conflict, check_output, default_output_path};
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
use std::time::{Duration, Instant};

const USAGE: &str =
    "usage: cargo run --example verify_acceptance -- [--bundled-assets] [--out <dir>]";

/// Fixture PDFs the run touches. Each is SHA-256-checked before and
/// after everything: the pipeline must never modify its inputs.
const FIXTURE_PDFS: [&str; 10] = [
    "eng_scan.pdf",
    "hun_scan.pdf",
    "rotated_scan.pdf",
    "cropbox_scan.pdf",
    "encrypted_scan.pdf",
    "gen_eng.pdf",
    "gen_hun.pdf",
    "gen_hybrid.pdf",
    "gen_rotated.pdf",
    "gen_multi.pdf",
];

fn main() {
    let started = Instant::now();
    let out_dir = parse_args();

    println!(
        "chilli-pdf {} acceptance driver (issue #21)",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "  profile: {}",
        if cfg!(debug_assertions) {
            "dev"
        } else {
            "release"
        }
    );
    println!(
        "  bundle:  {}",
        if cfg!(chilli_real_assets) {
            "real assets (--cfg chilli_real_assets)"
        } else {
            "fixture placeholders"
        }
    );
    println!("  output:  {}", out_dir.display());

    if !cfg!(chilli_real_assets) {
        eprintln!();
        eprintln!("ERROR: this build uses the fixture asset bundle, so OCR would run");
        eprintln!("against placeholder bytes and prove nothing. Fetch the real bundle");
        eprintln!("first (dev-time network), then rebuild and re-run:");
        eprintln!();
        eprintln!("    scripts/fetch-assets");
        eprintln!("    cargo run --example verify_acceptance -- [--out <dir>]");
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
    report.record(
        "bundle: bundled-assets mode (no scratch/system fallback)",
        check_bundled_assets(),
    );
    report.record("bundle: real assets present", check_real_bundle());

    let ctx = Ctx {
        converter: Arc::new(PipelineConverter::production()),
        out: out_dir.clone(),
    };
    report.record(
        "convert: eng_scan + English",
        check_convert_eng_english(&ctx),
    );
    report.record(
        "convert: hun_scan + Hungarian (ő/ű)",
        check_convert_hun_hungarian(&ctx),
    );
    report.record(
        "convert: eng_scan + English+Hungarian preset",
        check_convert_mixed(&ctx),
    );
    report.record(
        "convert: gen_hybrid skips already-text page",
        check_convert_hybrid(&ctx),
    );
    report.record(
        "convert: rotated_scan keeps /Rotate 90",
        check_convert_rotated(&ctx),
    );
    report.record(
        "convert: cropbox_scan keeps CropBox",
        check_convert_cropbox(&ctx),
    );
    report.record(
        "convert: gen_multi runs all five pages",
        check_convert_multi(&ctx),
    );
    report.record(
        "convert: Unicode path + default output name",
        check_convert_unicode(&ctx),
    );
    report.record("reject: encrypted scan", check_reject_encrypted(&ctx));
    report.record("reject: signed scan", check_reject_signed(&ctx));
    report.record("reject: malformed file", check_reject_malformed(&ctx));
    report.record("reject: output == input", check_reject_same_as_input(&ctx));
    report.record(
        "collision: existing output asks, then replaces",
        check_output_collision(&ctx),
    );
    report.record(
        "cancel: mid-document leaves no trace",
        check_cancel_mid_document(&ctx),
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

/// Parse `[--bundled-assets] [--out <dir>]`; the default scratch tree
/// lives under `target/`. `--bundled-assets` is the default (and only)
/// acceptance mode — accepted so scripts can state the contract
/// explicitly; there is deliberately no flag that would weaken it.
fn parse_args() -> PathBuf {
    let mut args = env::args_os().skip(1);
    let mut out: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--help" | "-h" => {
                println!("{USAGE}");
                exit(0);
            }
            "--bundled-assets" => {}
            "--out" => {
                out = Some(args.next().map(PathBuf::from).unwrap_or_else(|| die(USAGE)));
            }
            other => die(&format!("unknown argument {other:?} — {USAGE}")),
        }
    }
    out.unwrap_or_else(|| repo_path("target/verify-acceptance"))
}

/// Absolute path to `relative` inside the crate, wherever cargo runs us.
fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Absolute path to a committed fixture.
fn fixture_path(name: &str) -> PathBuf {
    repo_path("tests/fixtures").join(name)
}

fn die(message: &str) -> ! {
    eprintln!("verify_acceptance: {message}");
    exit(1);
}

/// Per-check bookkeeping; every check records exactly one outcome with
/// its evidence lines (PASS) or its failure reason (FAIL).
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

/// Everything the checks share: one production converter (the real
/// engines live on its actor thread) and the scratch root.
struct Ctx {
    converter: Arc<PipelineConverter>,
    out: PathBuf,
}

impl Ctx {
    /// A fresh scratch subdirectory `<out>/<name>`.
    fn scratch(&self, name: &str) -> Result<PathBuf, String> {
        let dir = self.out.join(name);
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(dir)
    }

    /// Copy a fixture into `dir` under its own name, so conversions never
    /// even point at the committed file.
    fn copy_fixture(&self, name: &str, dir: &Path) -> Result<PathBuf, String> {
        let input = dir.join(name);
        fs::copy(fixture_path(name), &input)
            .map_err(|e| format!("cannot copy fixture {name}: {e}"))?;
        Ok(input)
    }
}

// --- conversions ----------------------------------------------------------

/// One finished conversion: the paths, the event stream and the
/// structural facts `verify_structure` asserted along the way.
struct Conversion {
    input: PathBuf,
    output: PathBuf,
    events: Vec<WorkerEvent>,
    structure: Structure,
}

impl Conversion {
    /// The decoded overlay text of `page` (0-based); fails the check
    /// when the page was left without an overlay.
    fn overlay(&self, page: usize) -> Result<&str, String> {
        self.structure
            .overlays
            .get(page)
            .and_then(|text| text.as_deref())
            .ok_or_else(|| format!("page {} carries no overlay text", page + 1))
    }

    /// Evidence lines every conversion check starts from.
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
/// directory under the conventional `<stem>_searchable.pdf` name.
fn convert_fixture(
    ctx: &Ctx,
    dir_name: &str,
    fixture: &str,
    language: LanguagePreset,
    pages: usize,
) -> Result<Conversion, String> {
    let dir = ctx.scratch(dir_name)?;
    let input = ctx.copy_fixture(fixture, &dir)?;
    let output = dir.join(format!(
        "{}_searchable.pdf",
        fixture.strip_suffix(".pdf").unwrap_or(fixture)
    ));
    let events = run_to_done(
        ctx,
        Job {
            input: input.clone(),
            output: output.clone(),
            language,
        },
        pages,
    )?;
    let structure = verify_structure(&input, &output)?;
    if structure.pages != pages {
        return Err(format!("expected {pages} pages, found {}", structure.pages));
    }
    Ok(Conversion {
        input,
        output,
        events,
        structure,
    })
}

/// Start a job, wait for it to end, and require the exact happy-path
/// event stream: one `Progress` per page, then `Done`.
fn run_to_done(ctx: &Ctx, job: Job, total_pages: usize) -> Result<Vec<WorkerEvent>, String> {
    let events = convert(ctx, job)?;
    let mut expected: Vec<WorkerEvent> = (1..=total_pages)
        .map(|page| WorkerEvent::Progress { page, total_pages })
        .collect();
    expected.push(WorkerEvent::Done);
    if events != expected {
        return Err(format!(
            "unexpected event stream {events:?} (wanted {expected:?})"
        ));
    }
    Ok(events)
}

/// Start a job and block until its terminal event lands.
fn convert(ctx: &Ctx, job: Job) -> Result<Vec<WorkerEvent>, String> {
    let (events, inbox) = mpsc::channel();
    let mut worker = Arc::clone(&ctx.converter).start(job, events);
    worker.join();
    Ok(inbox.try_iter().collect())
}

/// The event stream of a rejected job: exactly one `Failed` and nothing
/// before it (validation fires before any page is touched).
fn single_failure(events: &[WorkerEvent]) -> Result<String, String> {
    let mut failures = Vec::new();
    for event in events {
        match event {
            WorkerEvent::Failed(message) => failures.push(message.clone()),
            WorkerEvent::Progress { .. } | WorkerEvent::Done | WorkerEvent::Cancelled => {
                return Err(format!(
                    "expected one Failed terminal event, got {events:?}"
                ));
            }
        }
    }
    if failures.len() != 1 {
        return Err(format!("expected exactly one Failed event, got {events:?}"));
    }
    Ok(failures.remove(0))
}

fn word_count(text: &str) -> usize {
    text.split(' ').filter(|word| !word.is_empty()).count()
}

fn require(condition: bool, message: String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message) }
}

/// No `.chilli-pdf-tmp` residue may remain in `dir`.
fn require_clean(dir: &Path) -> Result<(), String> {
    let residue = temp_residue(dir);
    require(
        residue.is_empty(),
        format!("temp files left behind: {residue:?}"),
    )
}

/// The bundled-assets acceptance check (issue #42): prove the run uses
/// ONLY the per-user extracted bundle. The #21 verification run once
/// passed while placeholder engine bytes shipped, because a system
/// fallback was providing the real engines behind the pipeline's back —
/// this check closes that evidence gap:
///
/// (a) the dev/test escape-hatch env vars must not be set, so nothing
///     outside the bundle can influence engine resolution;
/// (b) `ensure_assets()` — the exact call `PipelineConverter::production()`
///     makes — must extract and checksum-verify the bundle per-user;
/// (c) the extracted Tesseract/Leptonica bytes must NOT match the
///     committed fixture placeholder checksums (the canonical placeholder
///     definition, read from the fixture manifest that is always
///     committed regardless of bundle mode).
fn check_bundled_assets() -> Result<Vec<String>, String> {
    let mut evidence = Vec::new();

    for var in [
        "CHILLI_PDFIUM_LIB",
        "CHILLI_OCR_TESSERACT_LIB",
        "CHILLI_OCR_TESSDATA",
    ] {
        if let Some(value) = env::var_os(var) {
            return Err(format!(
                "{var} is set ({value:?}) — the acceptance run must use ONLY the \
                 bundled assets; unset it and re-run"
            ));
        }
    }
    evidence.push(
        "escape-hatch env vars unset (CHILLI_PDFIUM_LIB, CHILLI_OCR_TESSERACT_LIB, \
         CHILLI_OCR_TESSDATA)"
            .to_owned(),
    );

    let extracted = ensure_assets().map_err(|e| format!("ensure_assets() failed: {e}"))?;
    let dir = extracted.dir().to_path_buf();
    evidence.push(format!("per-user extracted bundle: {}", dir.display()));
    for library in [
        NativeLib::Pdfium,
        NativeLib::Leptonica,
        NativeLib::Tesseract,
    ] {
        let path = extracted
            .library_path(library)
            .map_err(|e| format!("cannot resolve {library:?} path: {e}"))?;
        let size = fs::metadata(&path)
            .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
            .len();
        evidence.push(format!(
            "{}: {} ({size} B)",
            name_of(library),
            path.display()
        ));
    }

    // The committed fixture manifest is the canonical placeholder
    // definition (embedded at compile time regardless of bundle mode).
    let fixture_manifest = AssetManifest::parse(include_str!("../assets/manifest.txt"))
        .map_err(|e| format!("cannot parse the committed fixture manifest: {e}"))?;
    for library in [NativeLib::Leptonica, NativeLib::Tesseract] {
        let path = extracted
            .library_path(library)
            .map_err(|e| format!("cannot resolve {library:?} path: {e}"))?;
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let slot = library.platform_file_name();
        let placeholder = fixture_manifest
            .entry(slot)
            .ok_or_else(|| format!("fixture manifest has no entry for slot {slot:?}"))?;
        if digest == hex::encode(placeholder.sha256) {
            return Err(format!(
                "{} is byte-identical to the committed fixture placeholder \
                 (sha256 {digest}) — the bundled assets are NOT the real engine \
                 (issue #42); re-run scripts/fetch-assets with real pins",
                path.display()
            ));
        }
        evidence.push(format!(
            "sha256 of extracted {slot} ({size} B) differs from the fixture \
             placeholder — real engine bytes",
            size = bytes.len()
        ));
    }

    Ok(evidence)
}

/// Human-readable name for evidence lines.
fn name_of(library: NativeLib) -> &'static str {
    match library {
        NativeLib::Pdfium => "pdfium",
        NativeLib::Tesseract => "tesseract",
        NativeLib::Leptonica => "leptonica",
    }
}

fn check_real_bundle() -> Result<Vec<String>, String> {
    let mut evidence = Vec::new();
    for entry in manifest().entries() {
        evidence.push(format!(
            "manifest: {:<26} {:>9} B  sha256:{}",
            entry.name,
            entry.size,
            hex::encode(entry.sha256)
        ));
    }
    // The fixture placeholders are 1–2 KB; the real libraries and models
    // are orders of magnitude larger. Anything placeholder-sized fails.
    for (needle, min_bytes) in [
        ("libpdfium", 1_000_000),
        ("libtesseract", 100_000),
        ("libleptonica", 100_000),
        ("eng.traineddata", 100_000),
        ("hun.traineddata", 100_000),
        ("pdf.ttf", 500),
    ] {
        let entry = manifest()
            .entries()
            .iter()
            .find(|entry| entry.name.contains(needle))
            .ok_or_else(|| format!("asset bundle has no entry matching {needle:?}"))?;
        if entry.size < min_bytes {
            return Err(format!(
                "asset {} is {} B — placeholder-sized, not a real {needle} (min {min_bytes} B)",
                entry.name, entry.size
            ));
        }
    }
    Ok(evidence)
}

fn check_convert_eng_english(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-eng-english",
        "eng_scan.pdf",
        LanguagePreset::English,
        1,
    )?;
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
    evidence.push(
        "text extraction / search / copy in real viewers is verified externally \
         (pdftotext, pypdf) — see docs/verification-report.md"
            .to_owned(),
    );
    Ok(evidence)
}

fn check_convert_hun_hungarian(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-hun-hungarian",
        "hun_scan.pdf",
        LanguagePreset::Hungarian,
        1,
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
    // Mojibake evidence: no replacement char and no decomposed accents
    // (combining diaeresis / double acute) where precomposed ő/ű belong.
    require(
        !text.contains('\u{FFFD}') && !text.contains('\u{0308}') && !text.contains('\u{030B}'),
        format!("mojibake in overlay text: {text:?}"),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(format!("decoded overlay text: {text:?}"));
    evidence.push("ő (U+0151) and ű (U+0171) present as single precomposed characters".to_owned());
    Ok(evidence)
}

fn check_convert_mixed(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-eng-hun-preset",
        "eng_scan.pdf",
        LanguagePreset::EnglishHungarian,
        1,
    )?;
    let text = conv.overlay(0)?;
    require(
        text.contains("fox"),
        format!("mixed preset failed to read the English scan: {text:?}"),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(format!(
        "preset EnglishHungarian (eng+hun models together), decoded text: {text:?}"
    ));
    Ok(evidence)
}

fn check_convert_hybrid(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-hybrid",
        "gen_hybrid.pdf",
        LanguagePreset::English,
        2,
    )?;
    // Page 1 already carries a real text layer and must be skipped
    // verbatim: identical content streams and no overlay ops at all.
    let (before, after) = conv.structure.stream_counts[0];
    require(
        before == after && conv.structure.overlays[0].is_none(),
        "the already-searchable page 1 was modified".to_owned(),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(
        "page 1 (real text layer): content streams byte-identical, no overlay — skipped".to_owned(),
    );
    match &conv.structure.overlays[1] {
        Some(text) => evidence.push(format!(
            "page 2 (scanned): overlay appended, {} words: {text:?}",
            word_count(text)
        )),
        None => evidence.push(
            "page 2 (scanned): OCR ran but the synthetic pattern yielded no \
             words; the page was left untouched"
                .to_owned(),
        ),
    }
    Ok(evidence)
}

fn check_convert_rotated(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-rotated",
        "rotated_scan.pdf",
        LanguagePreset::English,
        1,
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
    evidence.push(
        "word positions on the rotated page are verified externally \
         (pdftotext -bbox) — see docs/verification-report.md"
            .to_owned(),
    );
    Ok(evidence)
}

fn check_convert_cropbox(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-cropbox",
        "cropbox_scan.pdf",
        LanguagePreset::English,
        1,
    )?;
    require(
        conv.structure.crop_boxes[0] == Some([36.0, 36.0, 576.0, 756.0]),
        format!("CropBox not preserved: {:?}", conv.structure.crop_boxes),
    )?;
    require(
        conv.structure.media_boxes[0] == Some([0.0, 0.0, 612.0, 792.0]),
        format!("MediaBox not preserved: {:?}", conv.structure.media_boxes),
    )?;
    let text = conv.overlay(0)?;
    let words = word_count(text);
    require(
        words >= 3,
        format!("only {words} overlay words on the cropped page: {text:?}"),
    )?;
    let mut evidence = conv.evidence_lines();
    evidence.push(format!(
        "overlay inside the crop box ({words} words): {text:?}"
    ));
    evidence.push(
        "overlay positions vs the crop rectangle are verified externally \
         (pdftotext -bbox) — see docs/verification-report.md"
            .to_owned(),
    );
    Ok(evidence)
}

fn check_convert_multi(ctx: &Ctx) -> Result<Vec<String>, String> {
    let conv = convert_fixture(
        ctx,
        "convert-multi",
        "gen_multi.pdf",
        LanguagePreset::English,
        5,
    )?;
    let overlaid = conv
        .structure
        .overlays
        .iter()
        .filter(|text| text.is_some())
        .count();
    let mut evidence = conv.evidence_lines();
    evidence.push(format!(
        "5 pages processed page-by-page; {overlaid}/5 carry an overlay \
         (synthetic 64×64 patterns need not OCR to words)"
    ));
    Ok(evidence)
}

fn check_convert_unicode(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("convert-unicode")?;
    let folder = dir.join("mappák");
    fs::create_dir_all(&folder).map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
    let input = folder.join("Magyar szöveg.pdf");
    fs::copy(fixture_path("eng_scan.pdf"), &input)
        .map_err(|e| format!("cannot copy fixture: {e}"))?;
    let output = default_output_path(&input);
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    require(
        name.as_deref() == Some("Magyar szöveg_searchable.pdf"),
        format!("wrong default output name: {}", output.display()),
    )?;
    let events = run_to_done(
        ctx,
        Job {
            input: input.clone(),
            output: output.clone(),
            language: LanguagePreset::English,
        },
        1,
    )?;
    let structure = verify_structure(&input, &output)?;
    let text = structure
        .overlays
        .first()
        .and_then(|text| text.as_deref())
        .ok_or("the conversion produced no overlay")?;
    require(
        text.contains("fox"),
        format!("ground-truth words missing: {text:?}"),
    )?;
    Ok(vec![
        format!("input:  {}", input.display()),
        format!(
            "output: {} ({:?})",
            output.display(),
            fs::metadata(&output).map(|m| m.len())
        ),
        format!("events: {events:?}"),
        format!("decoded overlay text: {text:?}"),
    ])
}

// --- rejections -----------------------------------------------------------

/// Run one rejection case and require: a single `Failed` whose message
/// contains `needle`, no output file, and no temp residue.
fn expect_rejection(
    ctx: &Ctx,
    dir: &Path,
    input: PathBuf,
    needle: &str,
) -> Result<Vec<String>, String> {
    let output = dir.join("out.pdf");
    let events = convert(
        ctx,
        Job {
            input,
            output: output.clone(),
            language: LanguagePreset::English,
        },
    )?;
    let message = single_failure(&events)?;
    require(
        message.contains(needle),
        format!("user message does not mention {needle:?}: {message:?}"),
    )?;
    require(
        !output.exists(),
        "a rejected document still produced an output file".to_owned(),
    )?;
    require_clean(dir)?;
    Ok(vec![
        format!("user-facing message: {message:?}"),
        format!("no output, no temp residue in {}", dir.display()),
    ])
}

fn check_reject_encrypted(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("reject-encrypted")?;
    let input = ctx.copy_fixture("encrypted_scan.pdf", &dir)?;
    expect_rejection(ctx, &dir, input, "password-protected")
}

/// A minimal, structurally plausible PDF carrying signature tokens
/// (the same shape as the signed fixtures in `src/validation.rs`'s
/// tests). The validation scan classifies it as DigitallySigned before
/// the pipeline opens anything.
fn signed_pdf_bytes() -> Vec<u8> {
    b"%PDF-1.4\n\
      1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
      2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
      3 0 obj\n<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 5] /Contents <00> >>\nendobj\n\
      xref\n0 1\n0000000000 65535 f\n\
      trailer\n<< /Size 1 /Root 1 0 R /SigFlags 3 >>\n\
      startxref\n9\n\
      %%EOF\n"
        .to_vec()
}

fn check_reject_signed(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("reject-signed")?;
    let input = dir.join("signed_scan.pdf");
    fs::write(&input, signed_pdf_bytes())
        .map_err(|e| format!("cannot write synthetic signed PDF: {e}"))?;
    expect_rejection(ctx, &dir, input, "digitally signed")
}

fn check_reject_malformed(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("reject-malformed")?;
    let input = dir.join("malformed.pdf");
    fs::write(&input, b"this is definitely not a pdf")
        .map_err(|e| format!("cannot write malformed file: {e}"))?;
    expect_rejection(ctx, &dir, input, "does not look like a valid PDF")
}

fn check_reject_same_as_input(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("reject-same-as-input")?;
    let input = ctx.copy_fixture("eng_scan.pdf", &dir)?;
    let before = fs::read(&input).map_err(|e| format!("cannot read input: {e}"))?;
    let events = convert(
        ctx,
        Job {
            input: input.clone(),
            output: input.clone(),
            language: LanguagePreset::English,
        },
    )?;
    let message = single_failure(&events)?;
    require(
        message.contains("never overwrites the original"),
        format!("user message does not mention the no-overwrite rule: {message:?}"),
    )?;
    let after = fs::read(&input).map_err(|e| format!("cannot re-read input: {e}"))?;
    require(
        before == after,
        "the input bytes changed on a failed conversion".to_owned(),
    )?;
    require_clean(&dir)?;
    Ok(vec![
        format!("user-facing message: {message:?}"),
        format!(
            "input byte-identical after the refusal: {} bytes",
            after.len()
        ),
    ])
}

// --- output collision and cancellation -------------------------------------

fn check_output_collision(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("collision")?;
    let input = ctx.copy_fixture("eng_scan.pdf", &dir)?;
    let output = dir.join("out.pdf");
    fs::write(&output, b"stale bytes from an earlier run")
        .map_err(|e| format!("cannot seed stale output: {e}"))?;

    // The GUI contract: an existing destination is reported before any
    // conversion starts, so the app can ask the user before replacing.
    let conflict = check_output(&input, &output);
    require(
        conflict == Err(Conflict::AlreadyExists(output.clone())),
        format!("check_output returned {conflict:?}"),
    )?;

    // The pipeline policy behind the GUI's "yes, replace": an explicit
    // run replaces the stale file transactionally.
    let events = run_to_done(
        ctx,
        Job {
            input: input.clone(),
            output: output.clone(),
            language: LanguagePreset::English,
        },
        1,
    )?;
    let bytes = fs::read(&output).map_err(|e| format!("cannot read output: {e}"))?;
    require(
        bytes.starts_with(b"%PDF"),
        "the stale output bytes were not replaced".to_owned(),
    )?;
    let structure = verify_structure(&input, &output)?;
    let overlay = structure.overlays.first().cloned().flatten();
    require(
        overlay.as_deref().is_some_and(|text| text.contains("fox")),
        format!("the replaced output lost its overlay: {overlay:?}"),
    )?;
    require_clean(&dir)?;
    Ok(vec![
        format!("events: {events:?}"),
        "check_output() → Err(AlreadyExists): the GUI asks before replacing".to_owned(),
        format!(
            "pipeline replaced the stale bytes transactionally; output is {} bytes",
            bytes.len()
        ),
    ])
}

fn check_cancel_mid_document(ctx: &Ctx) -> Result<Vec<String>, String> {
    let dir = ctx.scratch("cancel")?;
    let input = ctx.copy_fixture("gen_multi.pdf", &dir)?;
    let output = dir.join("out.pdf");

    let (events, inbox) = mpsc::channel();
    let mut worker = Arc::clone(&ctx.converter).start(
        Job {
            input: input.clone(),
            output: output.clone(),
            language: LanguagePreset::English,
        },
        events,
    );

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut collected = Vec::new();
    let mut cancel_sent = false;
    loop {
        let event = match inbox.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() > deadline {
                    return Err("timed out waiting for the job".to_owned());
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if matches!(event, WorkerEvent::Progress { page: 1, .. }) && !cancel_sent {
            worker.cancel();
            cancel_sent = true;
        }
        let terminal = matches!(
            event,
            WorkerEvent::Done | WorkerEvent::Failed(_) | WorkerEvent::Cancelled
        );
        collected.push(event);
        if terminal {
            break;
        }
    }
    worker.join();
    collected.extend(inbox.try_iter());

    require(
        cancel_sent,
        "the job finished before the first page; cancellation could not be exercised".to_owned(),
    )?;
    require(
        collected
            .iter()
            .any(|event| matches!(event, WorkerEvent::Cancelled)),
        format!("no Cancelled terminal event: {collected:?}"),
    )?;
    require(
        !collected
            .iter()
            .any(|event| matches!(event, WorkerEvent::Done)),
        format!("the job completed despite cancellation: {collected:?}"),
    )?;
    require(
        !collected
            .iter()
            .any(|event| matches!(event, WorkerEvent::Failed(_))),
        format!("cancellation surfaced as a failure: {collected:?}"),
    )?;
    require(
        !output.exists(),
        "the cancelled run published an output".to_owned(),
    )?;
    require_clean(&dir)?;
    Ok(vec![
        format!("events: {collected:?}"),
        "cancel requested after the first page: Cancelled terminal, \
         no output, no temp residue, input intact"
            .to_owned(),
    ])
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

/// Recursively collect `.chilli-pdf-tmp`-style temp files under `dir`.
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

/// Temp-file names directly inside `dir` (non-recursive).
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

// --- lopdf output walkers (patterns shared with tests/integration.rs) -----

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

        let before_media = box_numbers(&before_dict, b"MediaBox");
        let after_media = box_numbers(&after_dict, b"MediaBox");
        require(
            before_media == after_media,
            format!(
                "page {}: MediaBox changed: {before_media:?} → {after_media:?}",
                page + 1
            ),
        )?;
        structure.media_boxes.push(after_media);

        let before_crop = box_numbers(&before_dict, b"CropBox");
        let after_crop = box_numbers(&after_dict, b"CropBox");
        require(
            before_crop == after_crop,
            format!(
                "page {}: CropBox changed: {before_crop:?} → {after_crop:?}",
                page + 1
            ),
        )?;
        structure.crop_boxes.push(after_crop);

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
