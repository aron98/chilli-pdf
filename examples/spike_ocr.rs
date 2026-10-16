//! Spike harness for issue #6: prove the render -> OCR -> invisible-text-overlay
//! pipeline end-to-end.
//!
//! Usage:
//!     cargo run --example spike_ocr -- <input.pdf> <outdir> [langs]
//!
//! Pipeline per page:
//!   1. Render the page at 300 DPI with PDFium (runtime-bound shared library,
//!      path from $CHILLI_PDFIUM_LIB, default /tmp/opencode/pdfium/lib/libpdfium.so).
//!   2. Save the bitmap as a JPEG and run the Tesseract CLI on it, producing
//!      TSV word boxes (`level 5` rows), Tesseract's own native searchable PDF
//!      (kept as a strategy-comparison artifact) and plain text in one pass.
//!   3. Map every OCR word box from display space (bitmap pixels, y-down,
//!      rotation already applied by the renderer) back to page user space
//!      (points, y-up) using the page's /Rotate value and the boundary box
//!      the bitmap actually covers.
//!   4. Emit a hand-rolled overlay PDF with the same page boxes and one
//!      invisible text object per word (render mode 3), embedding the
//!      glyphless font Tesseract ships (/usr/share/tesseract-ocr/5/tessdata/pdf.ttf,
//!      override with $CHILLI_GLYPHLESS_TTF). Overlay technique mirrors what
//!      Tesseract's own TessPDFRenderer does: Type0/Identity-H font whose
//!      codes are UTF-16BE, a CIDToGIDMap stream mapping every code to the
//!      single invisible glyph (advance 500/1000), an identity ToUnicode CMap,
//!      and per-word Tz horizontal scaling so the word's advance matches its
//!      OCR bounding box.
//!   5. Merge the overlay into the original pages with the pikepdf helper
//!      `examples/merge_overlay.py` (appends the overlay content stream to each
//!      original page and copies the font resource, leaving everything else —
//!      including the page image — untouched). The merge is done in USER space:
//!      unlike `qpdf --overlay`, which stamps in display orientation (wrong for
//!      pages with /Rotate, where it bakes in a rotate+scale transform), appending
//!      the text ops to the original content stream keeps the overlay aligned for
//!      every rotation and cropbox.
//!
//! Everything lands in <outdir>: page_NN.jpg, tess_NN.tsv / .pdf / .txt,
//! overlay.pdf and output.pdf (the final searchable document).

use pdfium_render::prelude::*;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const DPI: f32 = 300.0;
const PX_PER_PT: f32 = DPI / 72.0;
const DEFAULT_PDFIUM_LIB: &str = "/tmp/opencode/pdfium/lib/libpdfium.so";
const DEFAULT_GLYPHLESS_TTF: &str = "/usr/share/tesseract-ocr/5/tessdata/pdf.ttf";

/// One Tesseract level-5 TSV row: a word box in display pixels (y-down).
struct OcrWord {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    conf: f32,
    text: String,
}

/// Everything the overlay PDF writer needs for one page, in user space points.
struct PageOverlay {
    /// MediaBox [left, bottom, right, top] of the original page.
    media: [f32; 4],
    /// CropBox [left, bottom, right, top]; written to the overlay page when it
    /// differs from the MediaBox so the overlay page geometry matches exactly.
    crop: Option<[f32; 4]>,
    /// Text content stream for this page (invisible words only).
    content: String,
}

fn main() {
    let started = Instant::now();
    let mut args = env::args_os().skip(1);
    let input = match args.next() {
        Some(a) => PathBuf::from(a),
        None => die("usage: spike_ocr <input.pdf> <outdir> [langs]"),
    };
    let outdir = match args.next() {
        Some(a) => PathBuf::from(a),
        None => die("usage: spike_ocr <input.pdf> <outdir> [langs]"),
    };
    let langs = args
        .next()
        .map(|a| a.into_string().unwrap_or_else(|_| die("invalid langs")))
        .unwrap_or_else(|| "eng+hun".to_string());
    let pdfium_lib =
        env::var("CHILLI_PDFIUM_LIB").unwrap_or_else(|_| DEFAULT_PDFIUM_LIB.to_string());
    let glyphless_ttf =
        env::var("CHILLI_GLYPHLESS_TTF").unwrap_or_else(|_| DEFAULT_GLYPHLESS_TTF.to_string());

    if !Path::new(&pdfium_lib).exists() {
        die(&format!("PDFium library not found at {pdfium_lib}"));
    }
    let ttf_bytes = match fs::read(&glyphless_ttf) {
        Ok(b) => b,
        Err(e) => die(&format!("cannot read glyphless font {glyphless_ttf}: {e}")),
    };

    fs::create_dir_all(&outdir)
        .unwrap_or_else(|e| die(&format!("cannot create {}: {e}", outdir.display())));

    let pdfium = Pdfium::new(
        Pdfium::bind_to_library(&pdfium_lib)
            .unwrap_or_else(|e| die(&format!("cannot bind PDFium from {pdfium_lib}: {e}"))),
    );
    let doc = pdfium
        .load_pdf_from_file(&input, None)
        .unwrap_or_else(|e| die(&format!("cannot load {}: {e}", input.display())));

    let mut overlays = Vec::new();
    for (index, page) in doc.pages().iter().enumerate() {
        let n = index + 1;
        let rotation = page
            .rotation()
            .unwrap_or_else(|e| die(&format!("page {n} rotation: {e}")));

        let media_box = boundary(&page, n);
        // FPDFPage_GetCropBox errors when the page defines no CropBox (as in
        // the plain fixtures); PDF 7.7.3.3 says the effective CropBox then
        // defaults to the MediaBox, so fall back explicitly.
        let crop_box = match page.boundaries().crop() {
            Ok(b) => {
                let r = b.bounds;
                [
                    r.left().value,
                    r.bottom().value,
                    r.right().value,
                    r.top().value,
                ]
            }
            Err(_) => media_box,
        };

        // Render at 300 DPI. PDFium applies /Rotate, so the bitmap shows the
        // page exactly as a viewer displays it.
        let t_render = Instant::now();
        let config = PdfRenderConfig::new().scale_page_by_factor(PX_PER_PT);
        let bitmap = page
            .render_with_config(&config)
            .unwrap_or_else(|e| die(&format!("page {n} render: {e}")));
        let image = bitmap
            .as_image()
            .unwrap_or_else(|e| die(&format!("page {n} bitmap: {e}")))
            .into_rgb8();
        let (bmp_w, bmp_h) = image.dimensions();
        let jpg_path = outdir.join(format!("page_{n:02}.jpg"));
        image
            .save(&jpg_path)
            .unwrap_or_else(|e| die(&format!("cannot write {}: {e}", jpg_path.display())));
        eprintln!(
            "page {n}: rendered {}x{} px in {} ms -> {}",
            bmp_w,
            bmp_h,
            t_render.elapsed().as_millis(),
            jpg_path.display()
        );

        // Which page box does the bitmap cover? PDFium's FPDF_RenderPage
        // renders one of them; detect empirically so the coordinate mapping
        // stays exact (see spike report). Display dims are rotation-aware:
        // displayed width/height swap for 90/270.
        let (disp_w_pt, disp_h_pt) = (page.width().value, page.height().value);
        let (media_user_w, media_user_h) =
            (media_box[2] - media_box[0], media_box[3] - media_box[1]);
        let (media_disp_w, media_disp_h) = match rotation {
            PdfPageRenderRotation::Degrees90 | PdfPageRenderRotation::Degrees270 => {
                (media_user_h, media_user_w)
            }
            _ => (media_user_w, media_user_h),
        };
        let crop_disp_px = (disp_w_pt * PX_PER_PT, disp_h_pt * PX_PER_PT);
        let media_disp_px = (media_disp_w * PX_PER_PT, media_disp_h * PX_PER_PT);
        let covered = if approx(bmp_w as f32, crop_disp_px.0)
            && approx(bmp_h as f32, crop_disp_px.1)
        {
            crop_box
        } else if approx(bmp_w as f32, media_disp_px.0) && approx(bmp_h as f32, media_disp_px.1) {
            media_box
        } else {
            die(&format!(
                "page {n}: bitmap {}x{} px matches neither crop {crop_disp_px:?} nor media {media_disp_px:?} display size",
                bmp_w, bmp_h
            ))
        };
        eprintln!(
            "page {n}: rotation={rotation:?} media={media_box:?} crop={crop_box:?} bitmap covers {} box",
            if covered == media_box {
                "media"
            } else {
                "crop"
            }
        );

        // One Tesseract pass: word boxes (TSV) + Tesseract's own searchable PDF
        // (strategy-comparison artifact) + plain text.
        let stem = outdir.join(format!("tess_{n:02}"));
        let tsv_path = outdir.join(format!("tess_{n:02}.tsv"));
        let txt_path = outdir.join(format!("tess_{n:02}.txt"));
        let tess_pdf_path = outdir.join(format!("tess_{n:02}.pdf"));
        let t_ocr = Instant::now();
        let status = Command::new("tesseract")
            .arg(&jpg_path)
            .arg(&stem)
            .arg("-l")
            .arg(&langs)
            .arg("--dpi")
            .arg(DPI.to_string())
            .args(["tsv", "pdf", "txt"])
            .status()
            .unwrap_or_else(|e| die(&format!("cannot spawn tesseract: {e}")));
        if !status.success() {
            die(&format!(
                "tesseract failed on page {n} with status {status}"
            ));
        }

        let words = parse_tsv(&tsv_path);
        let mean_conf = if words.is_empty() {
            0.0
        } else {
            words.iter().map(|w| w.conf).sum::<f32>() / words.len() as f32
        };
        eprintln!(
            "page {n}: tesseract produced {} words (mean conf {mean_conf:.1}) in {} ms; native PDF: {}",
            words.len(),
            t_ocr.elapsed().as_millis(),
            tess_pdf_path.display()
        );

        let ocr_text = fs::read_to_string(&txt_path).unwrap_or_default();
        println!(
            "--- page {n} OCR text ({}, {} words) ---",
            langs,
            words.len()
        );
        println!("{}", ocr_text.trim_end());

        // Display space (points, y-down, bitmap origin at the covered box's
        // displayed top-left) -> user space (points, y-up).
        let (cov_disp_w_pt, cov_disp_h_pt) = if covered == media_box {
            (media_disp_w, media_disp_h)
        } else {
            (disp_w_pt, disp_h_pt)
        };
        let kx = cov_disp_w_pt / bmp_w as f32;
        let ky = cov_disp_h_pt / bmp_h as f32;
        let (box_w, box_h) = (covered[2] - covered[0], covered[3] - covered[1]);

        // The overlay text must advance along the DISPLAYED reading
        // direction, which is not the user-space +x axis when the page is
        // rotated. Per /Rotate value, the text matrix rotation part is:
        //   None: text +x -> user +x          (1 0 0 1)
        //     90: text +x -> user +y, up -> -x (0 1 -1 0)
        //    180: text +x -> user -x, up -> -y (-1 0 0 -1)
        //    270: text +x -> user -y, up -> +x (0 -1 1 0)
        // Tesseract's own PDF renderer avoids this entirely by writing the
        // page in bitmap/display orientation with /Rotate absent, which is
        // exactly why its output cannot be stamped onto the original page.
        let tm = match rotation {
            PdfPageRenderRotation::None => "1 0 0 1",
            PdfPageRenderRotation::Degrees90 => "0 1 -1 0",
            PdfPageRenderRotation::Degrees180 => "-1 0 0 -1",
            PdfPageRenderRotation::Degrees270 => "0 -1 1 0",
        };

        let mut content = String::new();
        for word in &words {
            let dx = word.left * kx;
            let dy_bottom = (word.top + word.height) * ky; // baseline anchor
            let w_pt = word.width * kx;
            let h_pt = word.height * ky;

            let (a, b) = match rotation {
                PdfPageRenderRotation::None => (dx, box_h - dy_bottom),
                PdfPageRenderRotation::Degrees90 => (dy_bottom, dx),
                PdfPageRenderRotation::Degrees180 => (box_w - dx, dy_bottom),
                PdfPageRenderRotation::Degrees270 => (box_w - dy_bottom, box_h - dx),
            };
            let x = covered[0] + a;
            let y = covered[1] + b;

            // Glyphless font: every code advances 500/1000 em, so stretch
            // horizontally with Tz until the string advance equals the word
            // box width. The trailing space keeps plain-text extraction clean,
            // exactly like Tesseract's own PDF renderer.
            let mut utf16 = Vec::new();
            for c in word.text.chars().chain(std::iter::once(' ')) {
                let mut buf = [0u16; 2];
                utf16.extend_from_slice(c.encode_utf16(&mut buf));
            }
            if utf16.is_empty() {
                continue;
            }
            let n_codes = utf16.len() as f32;
            let font_size = h_pt.max(0.1);
            let tz = (100.0 * w_pt / (0.5 * font_size * n_codes)).clamp(10.0, 1000.0);
            let hex: String = utf16.iter().map(|u| format!("{u:04X}")).collect();
            content.push_str(&format!(
                "BT 3 Tr /ChilliOcrF1 {font_size:.2} Tf {tz:.2} Tz {tm} {x:.2} {y:.2} Tm <{hex}> Tj ET\n"
            ));
        }

        overlays.push(PageOverlay {
            media: media_box,
            crop: if crop_box != media_box {
                Some(crop_box)
            } else {
                None
            },
            content,
        });
    }

    let overlay_path = outdir.join("overlay.pdf");
    let pdf_bytes = build_overlay_pdf(&overlays, &ttf_bytes);
    fs::write(&overlay_path, &pdf_bytes)
        .unwrap_or_else(|e| die(&format!("cannot write {}: {e}", overlay_path.display())));
    eprintln!(
        "wrote {} ({} bytes)",
        overlay_path.display(),
        pdf_bytes.len()
    );

    // Stamp the overlay onto the original. Both documents share identical
    // Media/CropBoxes, so the user-space content lines up 1:1.
    let merge_py = env::var("CHILLI_MERGE_PY").unwrap_or_else(|_| "python3".to_string());
    let merge_script =
        env::var("CHILLI_MERGE_SCRIPT").unwrap_or_else(|_| "examples/merge_overlay.py".to_string());
    let output_path = outdir.join("output.pdf");
    let t_merge = Instant::now();
    let status = Command::new(&merge_py)
        .arg(&merge_script)
        .arg(&overlay_path)
        .arg(&input)
        .arg(&output_path)
        .status()
        .unwrap_or_else(|e| die(&format!("cannot spawn {merge_py}: {e}")));
    if !status.success() {
        die(&format!("merge_overlay.py failed with status {status}"));
    }
    eprintln!("merged overlay in {} ms", t_merge.elapsed().as_millis());
    println!(
        "searchable PDF: {} ({} pages, langs={langs}, total {} ms)",
        output_path.display(),
        overlays.len(),
        started.elapsed().as_millis()
    );
}

fn die(msg: &str) -> ! {
    eprintln!("spike_ocr: {msg}");
    std::process::exit(1);
}

fn boundary(page: &PdfPage, n: usize) -> [f32; 4] {
    let rect = page
        .boundaries()
        .media()
        .unwrap_or_else(|e| die(&format!("page {n} media box: {e}")))
        .bounds;
    [
        rect.left().value,
        rect.bottom().value,
        rect.right().value,
        rect.top().value,
    ]
}

fn approx(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= 2.0
}

fn parse_tsv(path: &Path) -> Vec<OcrWord> {
    let tsv = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut words = Vec::new();
    for line in tsv.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 12 || cols[0] != "5" {
            continue;
        }
        let conf: f32 = cols[10].parse().unwrap_or(-1.0);
        if conf < 0.0 {
            continue;
        }
        let text = cols[11].trim();
        if text.is_empty() {
            continue;
        }
        words.push(OcrWord {
            left: cols[6].parse().unwrap_or(0.0),
            top: cols[7].parse().unwrap_or(0.0),
            width: cols[8].parse().unwrap_or(0.0),
            height: cols[9].parse().unwrap_or(0.0),
            conf,
            text: text.to_string(),
        });
    }
    words
}

/// A minimal PDF 1.7 writer: enough for pages that carry invisible text built
/// from one embedded Type0 font. Mirrors the object layout of Tesseract's
/// TessPDFRenderer output (verified by dissecting that output with qpdf).
fn build_overlay_pdf(pages: &[PageOverlay], ttf: &[u8]) -> Vec<u8> {
    // Object numbering:
    //   1 catalog, 2 pages tree, 3 Type0 font, 4 CIDFontType2 descendant,
    //   5 ToUnicode CMap, 6 font descriptor, 7 CIDToGIDMap stream,
    //   8 FontFile2 stream, then per page i (0-based): 9+2i content stream,
    //   10+2i page object.
    let mut objects: Vec<Vec<u8>> = vec![vec![]; 8 + 2 * pages.len()];

    objects[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 10 + 2 * i))
        .collect();
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    )
    .into_bytes();

    objects[2] = b"<< /Type /Font /Subtype /Type0 /BaseFont /GlyphLessFont /Encoding /Identity-H /DescendantFonts [4 0 R] /ToUnicode 5 0 R >>".to_vec();

    objects[3] = b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /GlyphLessFont /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 6 0 R /DW 500 /CIDToGIDMap 7 0 R >>".to_vec();

    // Identity ToUnicode: codes are already UTF-16BE, so every code maps to
    // itself (surrogate pairs reassemble into astral characters on extraction).
    objects[4] = stream(
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identify-UCS def\n/CMapType 2 def\n1 begincodespacerange\n\
         <0000> <FFFF>\nendcodespacerange\n1 beginbfrange\n<0000> <FFFF> <0000>\n\
         endbfrange\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n"
            .as_slice(),
    );

    objects[5] = b"<< /Type /FontDescriptor /FontName /GlyphLessFont /Flags 5 /FontBBox [0 0 500 1000] /ItalicAngle 0 /Ascent 1000 /Descent -1 /CapHeight 1000 /StemV 80 /FontFile2 8 0 R >>".to_vec();

    // Every CID maps to GID 1, the single invisible glyph (advance 500/1000).
    // 65536 entries x 2 bytes, high-order byte first, exactly as Tesseract does.
    let mut cid_to_gid = Vec::with_capacity(65536 * 2);
    for _ in 0..65536 {
        cid_to_gid.extend_from_slice(&[0x00, 0x01]);
    }
    objects[6] = stream(&cid_to_gid);

    let mut font_file = stream(ttf);
    // Patch the FontFile2 dict to add /Length1 (uncompressed TTF size).
    let dict = format!("<< /Length {} /Length1 {} >>\n", ttf.len(), ttf.len());
    font_file.splice(0..stream_dict_len(&font_file), dict.into_bytes());
    objects[7] = font_file;

    // Per page i (0-based): content stream = object 9+2i, page dict = object 10+2i.
    for (i, page) in pages.iter().enumerate() {
        let content_obj = 9 + 2 * i;
        objects[8 + 2 * i] = stream(page.content.as_bytes());
        let mut dict = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [{:.2} {:.2} {:.2} {:.2}]",
            page.media[0], page.media[1], page.media[2], page.media[3]
        );
        if let Some(crop) = page.crop {
            dict.push_str(&format!(
                " /CropBox [{:.2} {:.2} {:.2} {:.2}]",
                crop[0], crop[1], crop[2], crop[3]
            ));
        }
        dict.push_str(&format!(
            " /Resources << /Font << /ChilliOcrF1 3 0 R >> >> /Contents {content_obj} 0 R >>"
        ));
        objects[9 + 2 * i] = dict.into_bytes();
    }

    // Serialize.
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0u64; objects.len() + 1];
    for (idx, obj) in objects.iter().enumerate() {
        let num = idx + 1;
        offsets[num] = out.len() as u64;
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(obj);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_at = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A length-only stream dictionary followed by raw stream data.
fn stream(data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

/// Length of the `<< /Length N >>` prefix produced by [`stream`].
fn stream_dict_len(s: &[u8]) -> usize {
    s.iter().position(|b| *b == b'\n').unwrap_or(s.len()) + 1
}
