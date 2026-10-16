//! PDF rendering and saving backed by PDFium.
//!
//! This module owns every interaction with the native PDFium library. The
//! rest of the crate (and the GUI) talks only to the [`PdfEngine`] /
//! [`PdfDocument`] traits and the plain-data types defined here — never to
//! `pdfium-render` types directly, so the backend stays swappable.
//!
//! # Native library loading
//!
//! PDFium is loaded **dynamically at runtime**; nothing is linked at build
//! time and no binary is ever committed to git. The absolute path of the
//! library is resolved by a [`PdfiumLibrarySource`]:
//!
//! * **Production** (asset-extraction layer, PR #24): injects
//!   [`FixedLibraryPath`] (or a custom resolver) pointing at the library
//!   extracted from the bundled assets.
//! * **Development / CI**: [`ScratchPdfium`] looks under the gitignored
//!   `scratch/` directory (with a `CHILLI_PDFIUM_LIB` override for
//!   integration tests). To fetch the binary once:
//!
//!   ```text
//!   curl -L -o pdfium.tgz https://github.com/bblanchon/pdfium-binaries/releases/download/v157.0.8100/pdfium-linux-x64.tgz
//!   tar -xzf pdfium.tgz -C scratch/pdfium
//!   ```
//!
//! # Page indices
//!
//! All page indices in the public API are **zero-based**; error messages
//! phrase them one-based the way a user would.
//!
//! # Threading
//!
//! PDFium binds at most **once per process** and is not thread-safe. Create
//! the engine on one worker thread and keep all PDF work there; the traits
//! deliberately do not require `Send`/`Sync`. Tests share a single engine
//! in a single test entry point for the same reason.

mod error;
mod mock;
mod render;

pub use error::PdfError;
pub use mock::{MockEngine, MockPageSpec};
pub use render::{PdfiumDocument, PdfiumEngine};

use std::fmt;
use std::path::{Path, PathBuf};

/// DPI used for rendering when the caller does not specify a preference.
pub const TARGET_DPI: u32 = 300;

/// Hard cap on rendered pixels (~40 megapixels) so a pathologically large
/// page degrades its DPI instead of exhausting memory.
pub const MAX_RENDER_PIXELS: f64 = 40_000_000.0;

/// Lowest DPI we are willing to render at; below this the page is rejected
/// with [`PdfError::PageTooLarge`] rather than producing a useless bitmap.
pub const MIN_RENDER_DPI: f64 = 72.0;

/// A single page's `/Rotate` value, clockwise degrees as displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageRotation {
    /// No rotation (`/Rotate 0` or absent).
    #[default]
    None,
    /// Rotated 90° clockwise.
    Degrees90,
    /// Rotated 180°.
    Degrees180,
    /// Rotated 270° clockwise.
    Degrees270,
}

impl PageRotation {
    /// The clockwise rotation in degrees.
    #[must_use]
    pub fn degrees(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Degrees90 => 90,
            Self::Degrees180 => 180,
            Self::Degrees270 => 270,
        }
    }

    /// Whether text/page content swaps width and height when displayed.
    #[must_use]
    pub fn is_sideways(self) -> bool {
        matches!(self, Self::Degrees90 | Self::Degrees270)
    }
}

impl fmt::Display for PageRotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}°", self.degrees())
    }
}

/// A rectangle in PDF user-space points (origin bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox {
    /// Left edge, in points.
    pub left: f64,
    /// Bottom edge, in points.
    pub bottom: f64,
    /// Right edge, in points.
    pub right: f64,
    /// Top edge, in points.
    pub top: f64,
}

impl PageBox {
    /// Width in points. Falls back to `0.0` if the box is degenerate.
    #[must_use]
    pub fn width(self) -> f64 {
        (self.right - self.left).max(0.0)
    }

    /// Height in points. Falls back to `0.0` if the box is degenerate.
    #[must_use]
    pub fn height(self) -> f64 {
        (self.top - self.bottom).max(0.0)
    }
}

/// The per-page geometry callers need to lay out and render a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageMetadata {
    /// The page's `/Rotate` value (clockwise, as displayed).
    pub rotation: PageRotation,
    /// The page's crop box, which defaults to the media box when the PDF
    /// does not define one (PDF 7.7.3.3).
    pub crop_box: PageBox,
}

impl PageMetadata {
    /// The page's on-screen size in points: the crop box dimensions, swapped
    /// when the page is displayed sideways.
    #[must_use]
    pub fn display_size(self) -> (f64, f64) {
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        if self.rotation.is_sideways() {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Caller preferences for one page render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Desired horizontal/vertical DPI. The engine targets this but lowers
    /// it — reporting the value it actually used — when the resulting bitmap
    /// would exceed [`MAX_RENDER_PIXELS`].
    pub target_dpi: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            target_dpi: TARGET_DPI,
        }
    }
}

/// A rendered page: 8-bit RGB pixels in row-major order plus the DPI the
/// engine actually managed to use.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedImage {
    /// Pixel width of the bitmap.
    pub width: u32,
    /// Pixel height of the bitmap.
    pub height: u32,
    /// DPI actually used for this render; at most `options.target_dpi`, and
    /// lower when the pixel budget forced a reduction.
    pub dpi: f64,
    /// RGB pixel bytes, row-major, `width * height * 3` long.
    pub pixels: Vec<u8>,
}

impl RenderedImage {
    /// Number of pixels in the bitmap.
    #[must_use]
    pub fn pixel_count(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Resolves the absolute path of the native PDFium library at runtime.
///
/// Production injects the asset-extraction layer's answer; development and
/// tests use [`ScratchPdfium`] or [`FixedLibraryPath`]. Implementations must
/// return **absolute** paths — the engine rejects anything else so a stray
/// working directory can never pick up a wrong library.
pub trait PdfiumLibrarySource {
    /// The absolute path of `libpdfium` for this platform, or `None` when
    /// no library is available.
    fn library_path(&self) -> Option<PathBuf>;
}

/// A hard-coded library path, e.g. the one produced by the production
/// asset-extraction layer (PR #24).
#[derive(Debug, Clone)]
pub struct FixedLibraryPath(PathBuf);

impl FixedLibraryPath {
    /// Wraps an absolute path.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl PdfiumLibrarySource for FixedLibraryPath {
    fn library_path(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

/// Development/test resolver: honours the `CHILLI_PDFIUM_LIB` override, then
/// looks under the gitignored `scratch/pdfium/` directory.
#[derive(Debug, Clone, Default)]
pub struct ScratchPdfium;

impl PdfiumLibrarySource for ScratchPdfium {
    fn library_path(&self) -> Option<PathBuf> {
        // No existence check here: the engine reports the searched path in
        // a LibraryUnavailable error, which needs to name it.
        if let Ok(override_path) = std::env::var("CHILLI_PDFIUM_LIB") {
            return Some(PathBuf::from(override_path));
        }
        Some(default_scratch_library_path())
    }
}

/// `<crate>/scratch/pdfium/lib/libpdfium.so` (`.dylib` on macOS, `.dll` on
/// Windows) — matching the layout of the bblanchon/pdfium-binaries archives.
///
/// Anchored to the crate root (absolute) because the engine refuses
/// relative library paths on principle.
pub(crate) fn default_scratch_library_path() -> PathBuf {
    let name = if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else if cfg!(target_os = "windows") {
        "pdfium.dll"
    } else {
        "libpdfium.so"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scratch")
        .join("pdfium")
        .join("lib")
        .join(name)
}

/// Compute the render scale (pixels per point) for a page of the given
/// display size, honouring the DPI target and the pixel budget.
///
/// Mirrors pdfium-render's own integer math — each axis is
/// `(points * scale).round()` in `f32` — so the bitmap we predict is the
/// bitmap we get. If the target DPI overshoots the budget the scale is
/// reduced until the pixel product fits; if even `MIN_RENDER_DPI` does not
/// fit, the page is rejected as too large.
pub(crate) fn capped_render_scale(
    width_pt: f64,
    height_pt: f64,
    target_dpi: u32,
    page: usize,
) -> Result<f64, PdfError> {
    if !width_pt.is_finite() || !height_pt.is_finite() || width_pt <= 0.0 || height_pt <= 0.0 {
        return Err(PdfError::InvalidPageGeometry { page });
    }
    // A zero target is a caller bug; clamp to the smallest sane DPI so the
    // math below stays well-defined instead of producing a zero-size bitmap.
    let target_dpi = target_dpi.max(1);
    let mut scale = f64::from(target_dpi) / 72.0;
    loop {
        let w_px = pdfium_axis_pixels(width_pt, scale);
        let h_px = pdfium_axis_pixels(height_pt, scale);
        if w_px == 0 || h_px == 0 {
            return Err(PdfError::InvalidPageGeometry { page });
        }
        if f64::from(w_px) * f64::from(h_px) <= MAX_RENDER_PIXELS {
            break;
        }
        // Even the floor DPI would exceed the budget.
        if scale <= MIN_RENDER_DPI / 72.0 {
            return Err(PdfError::PageTooLarge { page });
        }
        scale *= 0.99;
    }
    Ok(scale)
}

/// pdfium-render rounds each axis independently in `f32` before truncating
/// to `c_int`; replicate that exactly so predicted dimensions match.
pub(crate) fn pdfium_axis_pixels(points: f64, scale: f64) -> i32 {
    ((points as f32) * (scale as f32)).round() as i32
}

/// Read-only page inspection plus rendering, decoupled from the backend.
///
/// Implementations render pages off the UI thread; the document handle is
/// also the writable form used later to append the invisible text layer
/// (the OCR overlay milestone will re-save through [`PdfDocument::save_as`]).
pub trait PdfDocument<'a> {
    /// Number of pages in the document.
    fn page_count(&self) -> Result<usize, PdfError>;

    /// Rotation and crop box of one page (zero-based index).
    fn page_metadata(&self, page: usize) -> Result<PageMetadata, PdfError>;

    /// Render one page (zero-based index) to an RGB bitmap at roughly
    /// `options.target_dpi`, reporting the DPI actually used.
    fn render_page(&self, page: usize, options: RenderOptions) -> Result<RenderedImage, PdfError>;

    /// Save the document — rendered appearance unchanged — to `destination`,
    /// producing a file the OCR overlay can later extend.
    fn save_as(&self, destination: &Path) -> Result<(), PdfError>;
}

/// Factory for opening PDFs; the rest of the app only ever sees this trait,
/// never a `pdfium-render` type.
pub trait PdfEngine {
    /// Open the PDF at `path` for reading, rendering, and re-saving.
    ///
    /// Password-protected, encrypted, unreadable, and malformed files come
    /// back as typed [`PdfError`]s; deciding what to *do* with them is
    /// validation policy, not engine behavior.
    fn open_document<'s>(&'s self, path: &Path) -> Result<Box<dyn PdfDocument<'s> + 's>, PdfError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_degrees_match_pdf_spec() {
        assert_eq!(PageRotation::None.degrees(), 0);
        assert_eq!(PageRotation::Degrees90.degrees(), 90);
        assert_eq!(PageRotation::Degrees180.degrees(), 180);
        assert_eq!(PageRotation::Degrees270.degrees(), 270);
        assert!(PageRotation::Degrees90.is_sideways());
        assert!(!PageRotation::None.is_sideways());
    }

    #[test]
    fn display_size_swaps_for_sideways_rotation() {
        let meta = PageMetadata {
            rotation: PageRotation::Degrees90,
            crop_box: PageBox {
                left: 0.0,
                bottom: 0.0,
                right: 792.0,
                top: 612.0,
            },
        };
        assert_eq!(meta.display_size(), (612.0, 792.0));

        let upright = PageMetadata {
            rotation: PageRotation::None,
            crop_box: meta.crop_box,
        };
        assert_eq!(upright.display_size(), (792.0, 612.0));
    }

    #[test]
    fn default_render_options_target_300_dpi() {
        assert_eq!(RenderOptions::default().target_dpi, TARGET_DPI);
    }

    #[test]
    fn scale_hits_target_within_budget() {
        // US Letter at 300 DPI: 612*300/72 = 2550, 792*300/72 = 3300.
        let scale = capped_render_scale(612.0, 792.0, 300, 0).unwrap();
        assert!((scale - 300.0 / 72.0).abs() < 1e-9);
    }

    #[test]
    fn oversized_page_reduces_dpi_instead_of_exploding() {
        // A0: 3371 x 2384 pt at 300 DPI is ~139 megapixels, well over the
        // 40 MP budget, so the scale must come down instead of exploding.
        let scale = capped_render_scale(3371.0, 2384.0, 300, 0).unwrap();
        let w = pdfium_axis_pixels(3371.0, scale);
        let h = pdfium_axis_pixels(2384.0, scale);
        assert!(f64::from(w) * f64::from(h) <= MAX_RENDER_PIXELS);
        let dpi = scale * 72.0;
        assert!(dpi < 300.0, "dpi should be reduced, got {dpi}");
        assert!(dpi >= MIN_RENDER_DPI);
    }

    #[test]
    fn absurdly_large_page_is_rejected_cleanly() {
        // 30000 x 30000 pt (~416 x 416 in) can't fit the budget even at 72 DPI.
        let err = capped_render_scale(30000.0, 30000.0, 300, 3).unwrap_err();
        assert_eq!(err, PdfError::PageTooLarge { page: 3 });
    }

    #[test]
    fn degenerate_geometry_is_rejected() {
        assert_eq!(
            capped_render_scale(0.0, 792.0, 300, 0).unwrap_err(),
            PdfError::InvalidPageGeometry { page: 0 }
        );
        assert_eq!(
            capped_render_scale(f64::NAN, 792.0, 300, 0).unwrap_err(),
            PdfError::InvalidPageGeometry { page: 0 }
        );
    }

    #[test]
    fn fixed_library_source_returns_wrapped_path() {
        let src = FixedLibraryPath::new(PathBuf::from("/opt/libpdfium.so"));
        assert_eq!(src.library_path(), Some(PathBuf::from("/opt/libpdfium.so")));
    }

    #[test]
    fn scratch_library_source_names_an_absolute_path() {
        // The engine refuses relative library paths, so the dev resolver
        // must always produce an absolute candidate.
        let path = ScratchPdfium.library_path().expect("dev resolver");
        assert!(path.is_absolute(), "got {}", path.display());
        assert_eq!(path, default_scratch_library_path());
    }
}
