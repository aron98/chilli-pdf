//! Invisible-text overlay writer (issue #13).
//!
//! Adds a searchable text layer to scanned PDFs: for every OCR word, a
//! snippet of invisible PDF text (`Tr 3`) is placed exactly on the word's
//! rendered position, so users can select and copy the text under the
//! scan image — including Hungarian `ő`/`ű` — while the visual appearance
//! of the document is preserved byte for byte.
//!
//! # How it works
//!
//! 1. [`layout`] maps each word's display-pixel box (from the OCR module)
//!    through the page's `/Rotate` value and crop-box origin into PDF
//!    user space, and sizes the font (`Tf`) and horizontal stretch (`Tz`)
//!    to the box.
//! 2. [`content`] renders the laid-out words as PDF text operators in a
//!    content stream wrapped in `q … Q`.
//! 3. [`font`] builds the glyphless Type0 font object set — the same
//!    construction Tesseract ships: Identity-H encoding, an identity
//!    ToUnicode CMap, and a CIDToGIDMap that sends every code to the
//!    single outline-less glyph.
//! 4. [`merge`] loads the ORIGINAL PDF with `lopdf` (pdfium-render is a
//!    reader/render API and cannot write objects), appends each page's
//!    overlay stream to `/Contents`, registers the font under
//!    `/Resources`, and saves to the output path. Original content and
//!    image streams are never decoded or rewritten.
//!
//! # Wiring (issue #10 pipeline)
//!
//! The public surface is deliberately one call: collect a
//! [`PageMetadata`](crate::pdf::PageMetadata) and an
//! [`OcrPage`](crate::ocr::OcrPage) per page (e.g. via
//! [`PdfDocument::page_metadata`](crate::pdf::PdfDocument::page_metadata)
//! and the OCR module), pair them as [`OverlayPage`] in document order,
//! and pass them with the glyphless font bytes to [`write_overlay`]:
//!
//! ```no_run
//! use chilli_pdf::ocr::OcrPage;
//! use chilli_pdf::overlay::{write_overlay, OverlayPage};
//! use chilli_pdf::pdf::PageMetadata;
//!
//! # fn example(
//! #     input: std::path::PathBuf,
//! #     output: std::path::PathBuf,
//! #     ocr_pages: Vec<OcrPage>,
//! #     metadata: Vec<PageMetadata>,
//! #     font_bytes: Vec<u8>,
//! # ) -> Result<(), chilli_pdf::overlay::OverlayError> {
//! let pages: Vec<OverlayPage> =
//!     ocr_pages.into_iter().zip(metadata).map(OverlayPage::from).collect();
//! write_overlay(&input, &output, &pages, &font_bytes)?;
//! # Ok(())
//! # }
//! ```
//!
//! The font bytes come from the bundled assets layer in production
//! (issue #9); tests resolve them from `tests/fixtures/pdf.ttf`.

mod content;
mod error;
mod font;
mod layout;
mod merge;

pub use error::OverlayError;

use crate::ocr::OcrPage;
use crate::pdf::PageMetadata;
use std::path::Path;

/// One page's inputs to the overlay writer: the OCR result for that page
/// paired with the page's geometry, in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayPage {
    /// Recognized words in display-pixel space, as the OCR module
    /// produced them for this page.
    pub ocr: OcrPage,
    /// The page's `/Rotate` value and crop box.
    pub metadata: PageMetadata,
}

impl From<(OcrPage, PageMetadata)> for OverlayPage {
    fn from((ocr, metadata): (OcrPage, PageMetadata)) -> Self {
        Self { ocr, metadata }
    }
}

/// Write an invisible-text overlay for every page of the PDF at `input`
/// and save the merged, searchable document to `output`.
///
/// `pages` holds one [`OverlayPage`] per document page, in order;
/// `font_bytes` is a TrueType font program (the bundled glyphless font).
/// The output is a new file; `input` is only read.
///
/// Pages whose OCR result contains no drawable words are left untouched.
/// Encrypted inputs are refused (see [`OverlayError::EncryptedUnsupported`]).
pub fn write_overlay(
    input: &Path,
    output: &Path,
    pages: &[OverlayPage],
    font_bytes: &[u8],
) -> Result<(), OverlayError> {
    merge::merge_overlay(input, output, pages, font_bytes)
}

/// Compute the overlay content stream for a single page (wrapped in
/// `q … Q`), without touching any file.
///
/// Useful for tests that want to inspect the generated operators; the
/// merge path uses the same layout math.
pub fn page_overlay_content(
    ocr: &OcrPage,
    metadata: &PageMetadata,
) -> Result<Vec<u8>, OverlayError> {
    let words = layout::layout_page(ocr, metadata, 0)?;
    Ok(content::overlay_ops(&words))
}
