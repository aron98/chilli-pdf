//! An in-memory [`PdfEngine`] mock for tests, mirroring `MockConverter`.
//!
//! No native library, no filesystem reads: pages are described by
//! [`MockPageSpec`], renders are plain white bitmaps, and saves are recorded
//! (plus written as a tiny placeholder file) so callers can assert on them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::error::PdfError;
use super::{
    PageBox, PageMetadata, PageRotation, PdfDocument, PdfEngine, RenderOptions, RenderedImage,
    capped_render_scale, pdfium_axis_pixels,
};

/// The geometry and scripted content of one fake page.
#[derive(Debug, Clone, PartialEq)]
pub struct MockPageSpec {
    /// Crop box in points.
    pub crop_box: PageBox,
    /// `/Rotate` value.
    pub rotation: PageRotation,
    /// Text returned by [`PdfDocument::page_text`] for this page, so
    /// tests can script already-searchable pages for the skip-text
    /// heuristic. Empty by default (a page that needs OCR).
    pub text: String,
}

impl MockPageSpec {
    /// A default US Letter page, unrotated, with no embedded text.
    #[must_use]
    pub fn letter() -> Self {
        Self {
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

    /// The same page carrying a text layer that classifies as already
    /// searchable (see [`PdfDocument::page_text`], `crate::skip_text`).
    #[must_use]
    pub fn searchable() -> Self {
        Self {
            text: "The quick brown fox jumps over the lazy dog.".to_owned(),
            ..Self::letter()
        }
    }
}

/// The document a [`MockEngine`] hands out for any opened path.
#[derive(Debug)]
struct MockDocument {
    pages: Vec<MockPageSpec>,
    saves: Arc<Mutex<Vec<PathBuf>>>,
}

impl PdfDocument<'_> for MockDocument {
    fn page_count(&self) -> Result<usize, PdfError> {
        Ok(self.pages.len())
    }

    fn page_metadata(&self, page: usize) -> Result<PageMetadata, PdfError> {
        let spec = self.pages.get(page).ok_or(PdfError::PageOutOfBounds {
            page,
            page_count: self.pages.len(),
        })?;
        Ok(PageMetadata {
            rotation: spec.rotation,
            crop_box: spec.crop_box,
        })
    }

    fn render_page(&self, page: usize, options: RenderOptions) -> Result<RenderedImage, PdfError> {
        let spec = self.pages.get(page).ok_or(PdfError::PageOutOfBounds {
            page,
            page_count: self.pages.len(),
        })?;
        let (width_pt, height_pt) = PageMetadata {
            rotation: spec.rotation,
            crop_box: spec.crop_box,
        }
        .display_size();
        let scale = capped_render_scale(width_pt, height_pt, options.target_dpi, page)?;
        let width = pdfium_axis_pixels(width_pt, scale).max(0) as u32;
        let height = pdfium_axis_pixels(height_pt, scale).max(0) as u32;
        let dpi = f64::from(width) * 72.0 / width_pt;
        Ok(RenderedImage {
            width,
            height,
            dpi,
            // All-white pixels: enough for callers to check size and buffers.
            pixels: vec![255; (width as usize) * (height as usize) * 3],
        })
    }

    fn page_text(&self, page: usize) -> Result<String, PdfError> {
        let spec = self.pages.get(page).ok_or(PdfError::PageOutOfBounds {
            page,
            page_count: self.pages.len(),
        })?;
        Ok(spec.text.clone())
    }

    fn save_as(&self, destination: &Path) -> Result<(), PdfError> {
        self.saves
            .lock()
            .expect("mock save log poisoned")
            .push(destination.to_path_buf());
        std::fs::write(destination, b"%PDF-mock\n").map_err(|_| PdfError::SaveFailed {
            path: destination.to_path_buf(),
        })
    }
}

/// A fake [`PdfEngine`]: every opened path yields the same scripted pages.
///
/// Clone freely in tests — saves made through any clone are visible in
/// `saves()` on all of them.
#[derive(Debug, Clone)]
pub struct MockEngine {
    pages: Vec<MockPageSpec>,
    open_error: Option<PdfError>,
    saves: Arc<Mutex<Vec<PathBuf>>>,
}

impl MockEngine {
    /// An engine whose documents have `count` US Letter pages.
    #[must_use]
    pub fn with_pages(count: usize) -> Self {
        Self {
            pages: vec![MockPageSpec::letter(); count],
            ..Self::default()
        }
    }

    /// An engine whose documents have exactly these pages, in order.
    ///
    /// Lets tests script per-page text (mixed scanned/digital documents)
    /// and geometry without any filesystem or native library.
    #[must_use]
    pub fn with_spec(pages: Vec<MockPageSpec>) -> Self {
        Self {
            pages,
            ..Self::default()
        }
    }

    /// An engine that rejects every open with this error (e.g. to test the
    /// GUI's handling of password-protected files).
    #[must_use]
    pub fn with_open_error(error: PdfError) -> Self {
        Self {
            open_error: Some(error),
            ..Self::default()
        }
    }

    /// Every destination passed to [`PdfDocument::save_as`] so far, in order.
    pub fn saves(&self) -> Vec<PathBuf> {
        self.saves.lock().expect("mock save log poisoned").clone()
    }
}

impl Default for MockEngine {
    fn default() -> Self {
        Self {
            pages: vec![MockPageSpec::letter()],
            open_error: None,
            saves: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl PdfEngine for MockEngine {
    fn open_document<'s>(&'s self, path: &Path) -> Result<Box<dyn PdfDocument<'s> + 's>, PdfError> {
        if let Some(error) = &self.open_error {
            return Err(error.clone());
        }
        // The mock ignores the path entirely.
        let _ = path;
        Ok(Box::new(MockDocument {
            pages: self.pages.clone(),
            saves: Arc::clone(&self.saves),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_engine_has_one_letter_page() {
        let engine = MockEngine::default();
        let doc = engine.open_document(Path::new("ignored.pdf")).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
        let meta = doc.page_metadata(0).unwrap();
        assert_eq!(meta.rotation, PageRotation::None);
        assert_eq!(meta.crop_box, MockPageSpec::letter().crop_box);
    }

    #[test]
    fn letter_renders_at_300_dpi_as_2550_by_3300() {
        let engine = MockEngine::default();
        let doc = engine.open_document(Path::new("x.pdf")).unwrap();
        let img = doc.render_page(0, RenderOptions::default()).unwrap();
        assert_eq!(img.width, 2550);
        assert_eq!(img.height, 3300);
        assert!(img.dpi > 299.9 && img.dpi < 300.1);
        assert_eq!(img.pixels.len(), 2550 * 3300 * 3);
    }

    #[test]
    fn configurable_dpi_is_honoured() {
        let engine = MockEngine::with_pages(2);
        let doc = engine.open_document(Path::new("x.pdf")).unwrap();
        let img = doc
            .render_page(1, RenderOptions { target_dpi: 150 })
            .unwrap();
        assert_eq!(img.width, 1275);
        assert_eq!(img.height, 1650);
    }

    #[test]
    fn out_of_bounds_page_is_rejected() {
        let engine = MockEngine::with_pages(1);
        let doc = engine.open_document(Path::new("x.pdf")).unwrap();
        assert_eq!(
            doc.page_metadata(1).unwrap_err(),
            PdfError::PageOutOfBounds {
                page: 1,
                page_count: 1
            }
        );
        assert_eq!(
            doc.render_page(5, RenderOptions::default()).unwrap_err(),
            PdfError::PageOutOfBounds {
                page: 5,
                page_count: 1
            }
        );
    }

    #[test]
    fn page_text_is_scripted_per_page() {
        let engine =
            MockEngine::with_spec(vec![MockPageSpec::searchable(), MockPageSpec::letter()]);
        let doc = engine.open_document(Path::new("mixed.pdf")).unwrap();
        assert_eq!(
            doc.page_text(0).unwrap(),
            "The quick brown fox jumps over the lazy dog."
        );
        assert_eq!(doc.page_text(1).unwrap(), "");
        assert_eq!(
            doc.page_text(2).unwrap_err(),
            PdfError::PageOutOfBounds {
                page: 2,
                page_count: 2
            }
        );
    }

    #[test]
    fn open_error_is_surfaced_verbatim() {
        let error = PdfError::PasswordProtected {
            path: PathBuf::from("secret.pdf"),
        };
        let engine = MockEngine::with_open_error(error.clone());
        // Box<dyn PdfDocument> is not Debug, so unwrap_err() is unavailable;
        // match by hand instead.
        match engine.open_document(Path::new("any.pdf")) {
            Err(err) => assert_eq!(err, error),
            Ok(_) => panic!("an engine with an open error must not open documents"),
        }
    }

    #[test]
    fn saves_are_recorded_and_written() {
        let dir = std::env::temp_dir().join("chilli_pdf_mock_saves");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.pdf");
        let engine = MockEngine::default();
        let doc = engine.open_document(Path::new("in.pdf")).unwrap();
        doc.save_as(&out).unwrap();
        assert_eq!(engine.saves(), vec![out.clone()]);
        assert!(out.is_file());

        assert_eq!(std::fs::read(&out).unwrap(), b"%PDF-mock\n");
    }

    #[test]
    fn absurd_mock_page_is_rejected_not_allocated() {
        // 30000 x 30000 pt cannot fit the budget even at 72 DPI; the mock
        // must refuse the render rather than build a giant bitmap.
        let engine = MockEngine {
            pages: vec![MockPageSpec {
                crop_box: PageBox {
                    left: 0.0,
                    bottom: 0.0,
                    right: 30000.0,
                    top: 30000.0,
                },
                rotation: PageRotation::None,
                text: String::new(),
            }],
            ..MockEngine::default()
        };
        let doc = engine.open_document(Path::new("absurd.pdf")).unwrap();
        assert_eq!(
            doc.render_page(0, RenderOptions::default()).unwrap_err(),
            PdfError::PageTooLarge { page: 0 }
        );
    }
}
