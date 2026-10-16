//! Integration tests exercising the real PDFium backend.
//!
//! These tests bind the native library downloaded into the gitignored
//! `scratch/` directory (see `src/pdf/mod.rs` for fetch instructions). When
//! the library is absent they **skip** — printing a note and returning — so
//! CI on machines without the binary stays green.
//!
//! PDFium can only be bound once per process, so every scenario runs in a
//! single test entry point sharing one engine.

use std::path::{Path, PathBuf};

use chilli_pdf::pdf::{
    MAX_RENDER_PIXELS, PageRotation, PdfEngine, PdfError, PdfiumEngine, RenderOptions,
    ScratchPdfium,
};

/// Path to a fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Directory for files the tests produce (minimal PDFs, saved copies).
fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("chilli-pdf-pdfium-tests");
    std::fs::create_dir_all(&dir).expect("test scratch dir");
    dir
}

/// Build a minimal one-page PDF with the given `/MediaBox` value string
/// (e.g. `"0 0 612 792"`). Hand-rolled so huge-page tests need no fixtures.
fn minimal_pdf(media_box: &str) -> Vec<u8> {
    let objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [{media_box}] \
             /Resources << >> /Contents 4 0 R >>"
        ),
        "<< /Length 0 >>\nstream\nendstream".into(),
    ];

    let mut pdf: Vec<u8> = b"%PDF-1.4\n%minimal\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len() as u32);
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len() as u32;
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

/// Assert two floats agree to within a PDF point's rounding slop.
fn assert_about(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 0.5,
        "{what}: expected ~{expected}, got {actual}"
    );
}

/// Open `path` and return the typed error. (The document handle is not
/// `Debug`, so `unwrap_err` is off-limits.)
fn open_err(engine: &PdfiumEngine, path: &Path) -> PdfError {
    match engine.open_document(path) {
        Ok(_) => panic!("expected opening {} to fail", path.display()),
        Err(err) => err,
    }
}

#[test]
fn real_pdfium_scenarios() {
    // Skip (don't fail) when no binary is available: CI machines that have
    // not fetched the PDFium archive must stay green.
    let Ok(engine) = PdfiumEngine::new(&ScratchPdfium) else {
        eprintln!(
            "skipping: no loadable PDFium binary under scratch/pdfium/lib/ \
             (or CHILLI_PDFIUM_LIB); see src/pdf/mod.rs for fetch instructions"
        );
        return;
    };

    plain_scan_scenario(&engine);
    rotated_scan_scenario(&engine);
    cropbox_scan_scenario(&engine);
    encrypted_pdf_scenario(&engine);
    invalid_file_scenario(&engine);
    missing_file_scenario(&engine);
    save_and_reload_scenario(&engine);
    huge_page_scenario(&engine);
    second_bind_scenario();
}

/// A plain US Letter scan: no CropBox (falls back to MediaBox), no
/// rotation, letter-sized render at the default 300 DPI.
fn plain_scan_scenario(engine: &PdfiumEngine) {
    let doc = engine.open_document(&fixture("eng_scan.pdf")).unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);

    let meta = doc.page_metadata(0).unwrap();
    assert_eq!(meta.rotation, PageRotation::None);
    assert_about(meta.crop_box.left, 0.0, "eng_scan cropbox left");
    assert_about(meta.crop_box.bottom, 0.0, "eng_scan cropbox bottom");
    assert_about(meta.crop_box.right, 612.0, "eng_scan cropbox right");
    assert_about(meta.crop_box.top, 792.0, "eng_scan cropbox top");

    let img = doc.render_page(0, RenderOptions::default()).unwrap();
    assert_eq!(img.width, 2550);
    assert_eq!(img.height, 3300);
    assert!(img.dpi > 299.0 && img.dpi < 301.0, "dpi: {}", img.dpi);
    assert_eq!(img.pixels.len(), 2550 * 3300 * 3);
    // Scanned text means the render is not a blank sheet.
    assert!(
        img.pixels.iter().any(|&b| b < 250),
        "rendered scan should contain dark pixels"
    );

    // The DPI target is configurable.
    let img = doc
        .render_page(0, RenderOptions { target_dpi: 150 })
        .unwrap();
    assert_eq!(img.width, 1275);
    assert_eq!(img.height, 1650);

    // Page indices are bounds-checked with a typed error.
    assert_eq!(
        doc.page_metadata(1).unwrap_err(),
        PdfError::PageOutOfBounds {
            page: 1,
            page_count: 1
        }
    );
}

/// A landscape page with `/Rotate 90` renders upright, matching how a
/// viewer displays it.
fn rotated_scan_scenario(engine: &PdfiumEngine) {
    let doc = engine.open_document(&fixture("rotated_scan.pdf")).unwrap();
    let meta = doc.page_metadata(0).unwrap();
    assert_eq!(meta.rotation, PageRotation::Degrees90);
    // MediaBox is 792 x 612; rotation displays it as 612 x 792.
    let (display_w, display_h) = meta.display_size();
    assert_about(display_w, 612.0, "rotated display width");
    assert_about(display_h, 792.0, "rotated display height");

    let img = doc.render_page(0, RenderOptions::default()).unwrap();
    assert_eq!(img.width, 2550);
    assert_eq!(img.height, 3300);
}

/// A page with an explicit CropBox renders only the cropped region.
fn cropbox_scan_scenario(engine: &PdfiumEngine) {
    let doc = engine.open_document(&fixture("cropbox_scan.pdf")).unwrap();
    let meta = doc.page_metadata(0).unwrap();
    assert_eq!(meta.rotation, PageRotation::None);
    assert_about(meta.crop_box.left, 36.0, "cropbox left");
    assert_about(meta.crop_box.bottom, 36.0, "cropbox bottom");
    assert_about(meta.crop_box.right, 576.0, "cropbox right");
    assert_about(meta.crop_box.top, 756.0, "cropbox top");

    // 540 x 720 pt at 300 DPI.
    let img = doc.render_page(0, RenderOptions::default()).unwrap();
    assert_eq!(img.width, 2250);
    assert_eq!(img.height, 3000);
}

/// A password-protected file surfaces as a typed error naming the file,
/// never the password or the contents.
fn encrypted_pdf_scenario(engine: &PdfiumEngine) {
    let path = fixture("encrypted_scan.pdf");
    let err = open_err(engine, &path);
    assert_eq!(err, PdfError::PasswordProtected { path });
    assert!(err.to_string().contains("password-protected"));
}

/// A file that exists but is not a PDF at all.
fn invalid_file_scenario(engine: &PdfiumEngine) {
    let path = scratch_dir().join("not-a-pdf.pdf");
    std::fs::write(&path, b"this is not a pdf, just some bytes").unwrap();
    let err = open_err(engine, &path);
    assert_eq!(err, PdfError::Malformed { path });
}

/// A missing file is an unreadable file, not a crash.
fn missing_file_scenario(engine: &PdfiumEngine) {
    let path = scratch_dir().join("does-not-exist.pdf");
    let err = open_err(engine, &path);
    assert_eq!(err, PdfError::FileUnreadable { path });
}

/// Saving produces a real PDF that re-opens and renders identically: the
/// round-trip the OCR overlay milestone will build on.
fn save_and_reload_scenario(engine: &PdfiumEngine) {
    let out = scratch_dir().join("roundtrip.pdf");
    {
        let doc = engine.open_document(&fixture("eng_scan.pdf")).unwrap();
        doc.save_as(&out).unwrap();
    }
    assert!(out.is_file());

    let doc = engine.open_document(&out).unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);
    let img = doc.render_page(0, RenderOptions::default()).unwrap();
    assert_eq!(img.width, 2550);
    assert_eq!(img.height, 3300);
    assert!(img.pixels.iter().any(|&b| b < 250));
}

/// A0-sized pages are rendered at a reduced DPI within the pixel budget,
/// and pages too big for even the minimum DPI are rejected with a clear
/// error instead of attempting an allocation that would OOM.
fn huge_page_scenario(engine: &PdfiumEngine) {
    let dir = scratch_dir();

    let a0 = dir.join("a0.pdf");
    std::fs::write(&a0, minimal_pdf("0 0 3371 2384")).unwrap();
    let doc = engine.open_document(&a0).unwrap();
    let img = doc.render_page(0, RenderOptions::default()).unwrap();
    assert!(
        f64::from(img.width) * f64::from(img.height) <= MAX_RENDER_PIXELS,
        "A0 render {}x{} exceeds the pixel budget",
        img.width,
        img.height
    );
    assert!(
        img.dpi < 300.0 && img.dpi >= 72.0,
        "A0 should render at a reduced DPI, got {}",
        img.dpi
    );

    let absurd = dir.join("absurd.pdf");
    std::fs::write(&absurd, minimal_pdf("0 0 30000 30000")).unwrap();
    let doc = engine.open_document(&absurd).unwrap();
    // Metadata is still readable; only rendering is refused.
    assert_eq!(doc.page_metadata(0).unwrap().rotation, PageRotation::None);
    assert_eq!(
        doc.render_page(0, RenderOptions::default()).unwrap_err(),
        PdfError::PageTooLarge { page: 0 }
    );
}

/// PDFium binds once per process: a second engine reports that cleanly.
fn second_bind_scenario() {
    let err = match PdfiumEngine::new(&ScratchPdfium) {
        Ok(_) => panic!("a second engine bind must fail"),
        Err(err) => err,
    };
    assert_eq!(err, PdfError::LibraryAlreadyBound);
}
