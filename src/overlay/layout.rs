//! Display-space → user-space layout math for the invisible-text overlay.
//!
//! OCR words arrive in **display pixel space** (the upright page as shown
//! to the user, y-down, origin top-left). The overlay text operators must
//! land in **PDF user space** (y-up, origin bottom-left of the crop box).
//! On pages with `/Rotate 90/180/270` the two spaces differ by a rotation,
//! so both the anchor point and the text matrix have to be transformed.
//!
//! The transform tables below are the ones proven in the issue #6 spike
//! (`docs/spike-report.md`, positioning-math section): the anchor is always
//! the display-space bottom-left corner of the word box, mapped through the
//! inverse of the page rotation, then offset by the crop-box origin.

use super::error::OverlayError;
use crate::ocr::{OcrPage, OcrWord};
use crate::pdf::{PageMetadata, PageRotation};

/// Everything needed to draw one overlay word, fully resolved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WordLayout {
    /// Font size in points — the word box height, so the text sits on the
    /// baseline at the box bottom with its ascent covering the box.
    pub(crate) font_size: f64,
    /// Horizontal scaling in percent (`Tz` operator): stretches the word's
    /// glyph advances to the box width. Clamped to `[10, 1000]` per the
    /// PDF operator's documented range.
    pub(crate) horizontal_scale: f64,
    /// The six operands of the `Tm` text matrix: rotation part per the page
    /// `/Rotate` value, translation part = the anchor in user space.
    pub(crate) text_matrix: [f64; 6],
    /// The word text as UTF-16BE hex digits, including one trailing space so
    /// plain-text extractors keep word breaks.
    pub(crate) hex_text: String,
}

/// Lower bound for `Tz` (PDF caps horizontal scaling at 10%).
const MIN_HORIZONTAL_SCALE: f64 = 10.0;
/// Upper bound for `Tz` (PDF caps horizontal scaling at 1000%).
const MAX_HORIZONTAL_SCALE: f64 = 1000.0;

/// Tolerance when checking that the OCR bitmap size matches the page
/// geometry at the reported DPI: 2 pixels plus 2%, absorbing pdfium's
/// per-axis rounding on huge pages.
const GEOMETRY_TOLERANCE: f64 = 2.0;

/// Lay out every word on one page.
///
/// `page` is the zero-based page index used in error messages. Words with
/// degenerate or empty boxes are skipped (they cannot size or place text).
pub(crate) fn layout_page(
    ocr: &OcrPage,
    metadata: &PageMetadata,
    page: usize,
) -> Result<Vec<WordLayout>, OverlayError> {
    if ocr.dpi == 0 {
        return Err(OverlayError::OcrGeometryMismatch { page });
    }
    let (display_w_pt, display_h_pt) = metadata.display_size();
    if !display_w_pt.is_finite()
        || !display_h_pt.is_finite()
        || display_w_pt <= 0.0
        || display_h_pt <= 0.0
    {
        return Err(OverlayError::InvalidPageGeometry { page });
    }
    check_ocr_matches_geometry(ocr, display_w_pt, display_h_pt, page)?;

    let pixels_per_point = f64::from(ocr.dpi) / 72.0;
    let crop = metadata.crop_box;
    let words = ocr.words().filter(|word| is_drawable(word)).map(|word| {
        layout_word(
            word,
            metadata.rotation,
            display_w_pt,
            display_h_pt,
            crop.left,
            crop.bottom,
            pixels_per_point,
        )
    });
    Ok(words.collect())
}

/// Reject OCR bitmaps whose pixel size does not match the page's display
/// size at the reported DPI — the pairs are mispaired and every computed
/// position would be wrong.
fn check_ocr_matches_geometry(
    ocr: &OcrPage,
    display_w_pt: f64,
    display_h_pt: f64,
    page: usize,
) -> Result<(), OverlayError> {
    for (expected_pt, actual_px) in [
        (display_w_pt, f64::from(ocr.width)),
        (display_h_pt, f64::from(ocr.height)),
    ] {
        let expected_px = expected_pt * f64::from(ocr.dpi) / 72.0;
        let tolerance = GEOMETRY_TOLERANCE + 0.02 * expected_px;
        if (actual_px - expected_px).abs() > tolerance {
            return Err(OverlayError::OcrGeometryMismatch { page });
        }
    }
    Ok(())
}

/// A word can only be drawn when it has text and a box with positive
/// width and height.
fn is_drawable(word: &OcrWord) -> bool {
    !word.text.is_empty() && word.rect.width() > 0 && word.rect.height() > 0
}

/// Compute one word's drawing parameters.
///
/// `display_w_pt`/`display_h_pt` are the DISPLAY dimensions in points,
/// `crop_left`/`crop_bottom` offset the anchor into user space, and
/// `pixels_per_point` converts OCR pixels to points.
fn layout_word(
    word: &OcrWord,
    rotation: PageRotation,
    display_w_pt: f64,
    display_h_pt: f64,
    crop_left: f64,
    crop_bottom: f64,
    pixels_per_point: f64,
) -> WordLayout {
    // Points per OCR pixel, and the word box in display-space points.
    let scale = 1.0 / pixels_per_point;
    let dx_left = f64::from(word.rect.left) * scale;
    let dy_bottom = f64::from(word.rect.bottom) * scale;
    let box_w = f64::from(word.rect.width()) * scale;
    let font_size = f64::from(word.rect.height()) * scale;

    // The anchor: the display-space bottom-left corner of the box mapped
    // through the inverse of the page rotation (spike §3 table), then
    // offset by the crop-box origin. W and H are the display dimensions
    // in points; the fourth element of each tuple is the text-matrix
    // rotation part for the forward rotation.
    let (a, b, rot) = match rotation {
        PageRotation::None => (dx_left, display_h_pt - dy_bottom, [1.0, 0.0, 0.0, 1.0]),
        PageRotation::Degrees90 => (dy_bottom, dx_left, [0.0, 1.0, -1.0, 0.0]),
        PageRotation::Degrees180 => (display_w_pt - dx_left, dy_bottom, [-1.0, 0.0, 0.0, -1.0]),
        PageRotation::Degrees270 => (
            display_h_pt - dy_bottom,
            display_w_pt - dx_left,
            [0.0, -1.0, 1.0, 0.0],
        ),
    };

    // UTF-16BE codes for the word plus one trailing space. The space keeps
    // plain-text extractors from gluing words together. Tz counts only the
    // word's own codes — excluding the trailing space — so the glyphs span
    // exactly the box width (the space overhangs past the right edge and
    // extractors trim it anyway). This matches Tesseract's PDF writer and
    // the spike's verified glyph-box formula
    // `[Tm.e, e + advance − trailing space] × …`.
    let mut codes: Vec<u16> = word.text.encode_utf16().collect();
    codes.push(u32::from(' ').try_into().unwrap_or(0x20));
    let code_count = f64::from(u32::try_from(codes.len() - 1).unwrap_or(1));
    let mut hex_text = String::with_capacity(codes.len() * 4);
    for code in codes {
        use std::fmt::Write as _;
        let _ = write!(hex_text, "{code:04X}");
    }

    // Stretch the advances (DW 500 = 0.5 em per code) to the box width.
    let raw_scale = 100.0 * box_w / (0.5 * font_size * code_count);
    let horizontal_scale = raw_scale.clamp(MIN_HORIZONTAL_SCALE, MAX_HORIZONTAL_SCALE);

    WordLayout {
        font_size,
        horizontal_scale,
        text_matrix: [
            rot[0],
            rot[1],
            rot[2],
            rot[3],
            crop_left + a,
            crop_bottom + b,
        ],
        hex_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::{OcrLine, OcrWord, Rect};
    use crate::pdf::PageBox;

    fn page_with(
        words: Vec<OcrWord>,
        width: u32,
        height: u32,
        dpi: u32,
        metadata: PageMetadata,
    ) -> (OcrPage, PageMetadata) {
        let line = OcrLine {
            rect: Rect::new(0, 0, width as i32, height as i32),
            words,
        };
        (
            OcrPage {
                width,
                height,
                dpi,
                lines: vec![line],
            },
            metadata,
        )
    }

    fn word(text: &str, l: i32, t: i32, r: i32, b: i32) -> OcrWord {
        OcrWord {
            text: text.into(),
            confidence: 90.0,
            rect: Rect::new(l, t, r, b),
        }
    }

    fn metadata(rotation: PageRotation, crop: PageBox) -> PageMetadata {
        PageMetadata {
            rotation,
            crop_box: crop,
        }
    }

    fn letter_crop() -> PageBox {
        PageBox {
            left: 0.0,
            bottom: 0.0,
            right: 612.0,
            top: 792.0,
        }
    }

    // Golden values below were computed by hand from the spike tables
    // (docs/spike-report.md): 300 DPI → 0.24 pt per pixel.

    #[test]
    fn rotation_none_maps_anchor_directly() {
        // Box (300,200)-(800,260) px on a 2550×3300 px letter page.
        let (page, meta) = page_with(
            vec![word("Hello", 300, 200, 800, 260)],
            2550,
            3300,
            300,
            metadata(PageRotation::None, letter_crop()),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        assert_eq!(words.len(), 1);
        let w = &words[0];
        // dx=72 pt, dy_bottom=62.4 pt → b=792−62.4=729.6.
        assert_eq!(w.text_matrix, [1.0, 0.0, 0.0, 1.0, 72.0, 729.6]);
        assert!((w.font_size - 14.4).abs() < 1e-9, "got {}", w.font_size);
        // Tz = 100·120 / (0.5·14.4·5) = 333.3̄ (5 text codes, trailing
        // space excluded so the glyphs span the 120 pt box width).
        assert!((w.horizontal_scale - 333.3333).abs() < 1e-3);
        assert_eq!(w.hex_text, "00480065006C006C006F0020");
    }

    #[test]
    fn rotation_90_swaps_axes() {
        // Crop box is the rotated landscape media box [0 0 792 612];
        // display is 612×792 pt. Box (600,900)-(1500,960) px at 300 DPI.
        let crop = PageBox {
            left: 0.0,
            bottom: 0.0,
            right: 792.0,
            top: 612.0,
        };
        let (page, meta) = page_with(
            vec![word("sideways", 600, 900, 1500, 960)],
            2550,
            3300,
            300,
            metadata(PageRotation::Degrees90, crop),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        let w = &words[0];
        // a=dy_bottom=960·0.24=230.4, b=dx_left=600·0.24=144. The products
        // are compared with a tiny epsilon: 960·0.24 rounds to
        // 230.399…98 in binary, not the literal 230.4.
        for (got, want) in w
            .text_matrix
            .iter()
            .zip([0.0, 1.0, -1.0, 0.0, 230.4, 144.0])
        {
            assert!((got - want).abs() < 1e-9, "got {got}, want {want}");
        }
        assert!((w.font_size - 14.4).abs() < 1e-9);
        // Tz = 100·216 / (0.5·14.4·8) = 375 (8 text codes; trailing space
        // excluded — Tesseract parity, verified against its eng_scan.pdf
        // output: "converts" 59 pt box → Tz 105.358 with n=8).
        assert!((w.horizontal_scale - 375.0).abs() < 1e-3);
        assert_eq!(w.hex_text, "007300690064006500770061007900730020");
    }

    #[test]
    fn rotation_180_flips_both_axes() {
        let (page, meta) = page_with(
            vec![word("flip", 100, 500, 400, 560)],
            2550,
            3300,
            300,
            metadata(PageRotation::Degrees180, letter_crop()),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        let w = &words[0];
        // a=612−24=588, b=dy_bottom=560·0.24=134.4.
        assert_eq!(w.text_matrix, [-1.0, 0.0, 0.0, -1.0, 588.0, 134.4]);
    }

    #[test]
    fn rotation_270_flips_swapped_axes() {
        let (page, meta) = page_with(
            vec![word("flip", 600, 900, 1500, 960)],
            2550,
            3300,
            300,
            metadata(
                PageRotation::Degrees270,
                PageBox {
                    left: 0.0,
                    bottom: 0.0,
                    right: 792.0,
                    top: 612.0,
                },
            ),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        let w = &words[0];
        // a=792−230.4=561.6, b=612−144=468.
        assert_eq!(w.text_matrix, [0.0, -1.0, 1.0, 0.0, 561.6, 468.0]);
    }

    #[test]
    fn crop_box_offsets_anchor() {
        // Crop box [36,36,576,756] → display 540×720 pt = 2250×3000 px at
        // 300 DPI. Box left=300 px → dx=72 pt, bottom=260 px → dy=62.4 pt;
        // b=720−62.4=657.6 → user=(36+72, 36+657.6)=(108, 693.6).
        let crop = PageBox {
            left: 36.0,
            bottom: 36.0,
            right: 576.0,
            top: 756.0,
        };
        let (page, meta) = page_with(
            vec![word("crop", 300, 200, 800, 260)],
            2250,
            3000,
            300,
            metadata(PageRotation::None, crop),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        let w = &words[0];
        assert_eq!(w.text_matrix, [1.0, 0.0, 0.0, 1.0, 108.0, 693.6]);
    }

    #[test]
    fn utf16_covers_hungarian_and_astral() {
        // ő = U+0151, ű = U+0171 must round-trip; U+1F600 becomes a
        // surrogate pair — both fall out of the identity CMap design.
        let (page, meta) = page_with(
            vec![word("őű", 100, 100, 400, 160)],
            2550,
            3300,
            300,
            metadata(PageRotation::None, letter_crop()),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        assert_eq!(words[0].hex_text, "015101710020");

        let (page, meta) = page_with(
            vec![word("😀", 100, 100, 400, 160)],
            2550,
            3300,
            300,
            metadata(PageRotation::None, letter_crop()),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        assert_eq!(words[0].hex_text, "D83DDE000020");
    }

    #[test]
    fn horizontal_scale_clamps_to_pdf_range() {
        // A 2 px wide, 100 px tall box at 300 DPI with 7 text glyphs: raw
        // Tz = 100·0.48 / (0.5·24·7) ≈ 0.57 → clamped up to 10. A box far
        // wider than its height gives raw Tz ≈ 5700 → clamped to 1000.
        let narrow = (2550, 3300, (0, 0, 2, 100));
        let wide = (2550, 3300, (0, 0, 20_000, 100));
        for (expected, (w, h, (l, t, r, b))) in [(10.0, narrow), (1000.0, wide)] {
            let (page, meta) = page_with(
                vec![word("clamped", l, t, r, b)],
                w,
                h,
                300,
                metadata(PageRotation::None, letter_crop()),
            );
            let words = layout_page(&page, &meta, 0).unwrap();
            assert_eq!(words[0].horizontal_scale, expected);
        }
    }

    #[test]
    fn degenerate_words_are_skipped() {
        let (page, meta) = page_with(
            vec![
                word("", 100, 100, 400, 160),     // no text
                word("flat", 100, 100, 400, 100), // zero height
                word("thin", 100, 100, 100, 160), // zero width
                word("ok", 100, 100, 400, 160),
            ],
            2550,
            3300,
            300,
            metadata(PageRotation::None, letter_crop()),
        );
        let words = layout_page(&page, &meta, 0).unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].hex_text, "006F006B0020");
    }

    #[test]
    fn zero_dpi_or_broken_geometry_is_rejected() {
        let (page, meta) = page_with(
            vec![word("x", 0, 0, 10, 10)],
            2550,
            3300,
            0,
            metadata(PageRotation::None, letter_crop()),
        );
        assert_eq!(
            layout_page(&page, &meta, 3).unwrap_err(),
            OverlayError::OcrGeometryMismatch { page: 3 }
        );

        let (page, meta) = page_with(
            vec![word("x", 0, 0, 10, 10)],
            2550,
            3300,
            300,
            metadata(
                PageRotation::None,
                PageBox {
                    left: 100.0,
                    bottom: 100.0,
                    right: 100.0,
                    top: 100.0, // degenerate crop box
                },
            ),
        );
        assert_eq!(
            layout_page(&page, &meta, 0).unwrap_err(),
            OverlayError::InvalidPageGeometry { page: 0 }
        );
    }

    #[test]
    fn mismatched_ocr_bitmap_size_is_rejected() {
        // 100×100 px is nowhere near letter at 300 DPI.
        let (page, meta) = page_with(
            vec![word("x", 0, 0, 10, 10)],
            100,
            100,
            300,
            metadata(PageRotation::None, letter_crop()),
        );
        assert_eq!(
            layout_page(&page, &meta, 1).unwrap_err(),
            OverlayError::OcrGeometryMismatch { page: 1 }
        );
    }
}
