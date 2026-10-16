//! The real [`PdfEngine`] implementation, backed by `pdfium-render`.
//!
//! Every `pdfium-render` type is confined to this file; callers see only
//! the plain-data types from [`super`]. The native library is bound
//! dynamically at runtime from an absolute path supplied by a
//! [`PdfiumLibrarySource`](super::PdfiumLibrarySource).

use std::path::{Path, PathBuf};

use pdfium_render::prelude::*;
// Our own trait is also called `PdfDocument` (imported below), so give the
// pdfium-render struct a distinct local name.
use pdfium_render::prelude::PdfDocument as PdfiumRenderDocument;

use super::error::PdfError;
use super::{
    PageBox, PageMetadata, PageRotation, PdfDocument, PdfEngine, PdfiumLibrarySource,
    RenderOptions, RenderedImage, capped_render_scale, default_scratch_library_path,
};

/// An engine that binds the native PDFium library on construction.
///
/// PDFium can only be bound once per process, so construct this exactly
/// once (on the worker thread that will own all PDF work).
pub struct PdfiumEngine {
    pdfium: Pdfium,
}

impl PdfiumEngine {
    /// Bind the native library resolved by `source` and return an engine.
    ///
    /// # Errors
    ///
    /// - [`PdfError::LibraryUnavailable`] when the source names no library
    ///   at all, or the named file does not exist.
    /// - [`PdfError::LibraryLoadFailed`] when the library exists but cannot
    ///   be loaded — including a relative path, which is rejected so a
    ///   stray working directory can never pick up the wrong binary.
    /// - [`PdfError::LibraryAlreadyBound`] if a second engine is attempted:
    ///   PDFium binds at most once per process.
    pub fn new(source: &dyn PdfiumLibrarySource) -> Result<Self, PdfError> {
        let path = source
            .library_path()
            .ok_or_else(|| PdfError::LibraryUnavailable {
                // The source has no candidate at all; name the conventional dev
                // location so the message stays actionable.
                path: default_scratch_library_path(),
            })?;
        if !path.is_absolute() {
            return Err(PdfError::LibraryLoadFailed { path });
        }
        if !path.is_file() {
            return Err(PdfError::LibraryUnavailable { path });
        }
        let bindings = Pdfium::bind_to_library(&path).map_err(|err| match err {
            PdfiumError::PdfiumLibraryBindingsAlreadyInitialized => PdfError::LibraryAlreadyBound,
            _ => PdfError::LibraryLoadFailed { path },
        })?;
        Ok(Self {
            pdfium: Pdfium::new(bindings),
        })
    }

    fn load(&self, path: &Path) -> Result<PdfiumRenderDocument<'_>, PdfError> {
        self.pdfium
            .load_pdf_from_file(path, None)
            .map_err(|err| map_load_error(err, path))
    }
}

impl PdfEngine for PdfiumEngine {
    fn open_document<'s>(&'s self, path: &Path) -> Result<Box<dyn PdfDocument<'s> + 's>, PdfError> {
        Ok(Box::new(PdfiumDocument {
            document: self.load(path)?,
            path: path.to_path_buf(),
        }))
    }
}

/// A PDF opened through [`PdfiumEngine`], exposing inspect/render/save via
/// the backend-independent [`PdfDocument`] trait.
pub struct PdfiumDocument<'a> {
    document: PdfiumRenderDocument<'a>,
    /// The file this document was loaded from, for error messages.
    path: PathBuf,
}

impl PdfiumDocument<'_> {
    /// Fetch a page after bounds-checking the zero-based index.
    fn page(&self, page: usize) -> Result<PdfPage<'_>, PdfError> {
        let count = self.page_count()?;
        if page >= count {
            return Err(PdfError::PageOutOfBounds {
                page,
                page_count: count,
            });
        }
        // Index is verified in range; pdfium-render only fails here on
        // internal inconsistency, which we surface as the same bounds error.
        self.document
            .pages()
            .get(page as i32)
            .map_err(|_| PdfError::PageOutOfBounds {
                page,
                page_count: count,
            })
    }
}

impl PdfDocument<'_> for PdfiumDocument<'_> {
    fn page_count(&self) -> Result<usize, PdfError> {
        let len = self.document.pages().len();
        if len < 0 {
            // PdfPages::len() is an i32; negative can only mean PDFium
            // disagreed with the document it just parsed.
            return Err(PdfError::Malformed {
                path: self.path.clone(),
            });
        }
        Ok(len as usize)
    }

    fn page_metadata(&self, page: usize) -> Result<PageMetadata, PdfError> {
        let pdf_page = self.page(page)?;
        let rotation = pdf_page
            .rotation()
            .map(|rotation| match rotation {
                PdfPageRenderRotation::None => PageRotation::None,
                PdfPageRenderRotation::Degrees90 => PageRotation::Degrees90,
                PdfPageRenderRotation::Degrees180 => PageRotation::Degrees180,
                PdfPageRenderRotation::Degrees270 => PageRotation::Degrees270,
            })
            .map_err(|_| PdfError::RenderFailed { page })?;

        // Per PDF 7.7.3.3 the crop box defaults to the media box; PDFium's
        // GetCropBox errors when the page has none, so fall back explicitly
        // before giving up on the page's own point size.
        let boundaries = pdf_page.boundaries();
        let crop_box = match boundaries.crop() {
            Ok(crop) => rect_to_page_box(crop.bounds),
            Err(_) => match boundaries.media() {
                Ok(media) => rect_to_page_box(media.bounds),
                Err(_) => PageBox {
                    left: 0.0,
                    bottom: 0.0,
                    right: f64::from(pdf_page.width().value),
                    top: f64::from(pdf_page.height().value),
                },
            },
        };

        let metadata = PageMetadata { rotation, crop_box };
        if !crop_box.width().is_finite()
            || !crop_box.height().is_finite()
            || crop_box.width() <= 0.0
            || crop_box.height() <= 0.0
        {
            return Err(PdfError::InvalidPageGeometry { page });
        }
        Ok(metadata)
    }

    fn render_page(&self, page: usize, options: RenderOptions) -> Result<RenderedImage, PdfError> {
        let pdf_page = self.page(page)?;

        // page.width()/height() are the DISPLAY size: crop box applied and
        // rotation swapped, matching what render produces pixels for.
        let width_pt = f64::from(pdf_page.width().value);
        let height_pt = f64::from(pdf_page.height().value);
        let scale = capped_render_scale(width_pt, height_pt, options.target_dpi, page)?;

        let config = PdfRenderConfig::new().scale_page_by_factor(scale as f32);
        let bitmap = pdf_page
            .render_with_config(&config)
            .map_err(|_| PdfError::RenderFailed { page })?;

        let width = bitmap.width().max(0) as u32;
        let height = bitmap.height().max(0) as u32;
        if width == 0 || height == 0 {
            return Err(PdfError::InvalidPageGeometry { page });
        }
        let image = bitmap
            .as_image()
            .map_err(|_| PdfError::RenderFailed { page })?
            .into_rgb8();

        // The DPI actually used: pdfium-render rounds each axis in f32, so
        // derive it from the real pixel width rather than the scale.
        let dpi = f64::from(width) * 72.0 / width_pt;
        Ok(RenderedImage {
            width,
            height,
            dpi,
            pixels: image.into_raw(),
        })
    }

    fn save_as(&self, destination: &Path) -> Result<(), PdfError> {
        self.document
            .save_to_file(destination)
            .map_err(|_| PdfError::SaveFailed {
                path: destination.to_path_buf(),
            })
    }
}

/// Translate a PDFium open failure into a typed, content-free error.
fn map_load_error(err: PdfiumError, path: &Path) -> PdfError {
    let path = path.to_path_buf();
    match err {
        PdfiumError::IoError(_) => PdfError::FileUnreadable { path },
        PdfiumError::PdfiumLibraryInternalError(internal) => match internal {
            PdfiumInternalError::PasswordError => PdfError::PasswordProtected { path },
            PdfiumInternalError::SecurityError => PdfError::EncryptionUnsupported { path },
            PdfiumInternalError::FileError => PdfError::FileUnreadable { path },
            _ => PdfError::Malformed { path },
        },
        _ => PdfError::Malformed { path },
    }
}

/// Convert a `pdfium-render` rectangle into our plain-data [`PageBox`].
fn rect_to_page_box(rect: PdfRect) -> PageBox {
    PageBox {
        left: f64::from(rect.left().value),
        bottom: f64::from(rect.bottom().value),
        right: f64::from(rect.right().value),
        top: f64::from(rect.top().value),
    }
}
