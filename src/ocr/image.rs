//! Raw page image handed to Tesseract.

use super::error::OcrError;

/// Pixel layout of an [`OcrImage`] buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// One byte per pixel, luminance.
    Gray,
    /// Three bytes per pixel, RGB order.
    Rgb,
    /// Four bytes per pixel, RGBA order.
    Rgba,
}

impl PixelFormat {
    /// Bytes per pixel, as Tesseract's `bytes_per_pixel` argument expects.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }
}

/// Resolution chilli-pdf renders pages at, and the value Tesseract is told
/// about when no other DPI is supplied.
pub const DEFAULT_DPI: u32 = 300;

/// A decoded page image in raw bytes, ready for recognition.
///
/// The buffer is borrowed (zero-copy): row-major, tightly packed, with no
/// per-row padding. The default resolution is [`DEFAULT_DPI`]; override it with
/// [`OcrImage::with_dpi`] when the source raster carries a different DPI.
#[derive(Debug, Clone, Copy)]
pub struct OcrImage<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    format: PixelFormat,
    dpi: u32,
}

impl<'a> OcrImage<'a> {
    /// Wrap a decoded image. The DPI defaults to [`DEFAULT_DPI`].
    pub const fn new(data: &'a [u8], width: u32, height: u32, format: PixelFormat) -> Self {
        Self {
            data,
            width,
            height,
            format,
            dpi: DEFAULT_DPI,
        }
    }

    /// Override the image resolution (dots per inch).
    pub const fn with_dpi(mut self, dpi: u32) -> Self {
        self.dpi = dpi;
        self
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    pub const fn dpi(&self) -> u32 {
        self.dpi
    }

    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Validate that the buffer matches the declared dimensions and return
    /// `(width, height, bytes_per_pixel, bytes_per_line)` as the `i32` values
    /// Tesseract expects.
    pub(crate) fn validate(&self) -> Result<(i32, i32, i32, i32), OcrError> {
        if self.width == 0 || self.height == 0 {
            return Err(OcrError::InvalidImage {
                message: "width and height must be non-zero".into(),
            });
        }
        if self.dpi == 0 {
            return Err(OcrError::InvalidImage {
                message: "dpi must be non-zero".into(),
            });
        }
        let bpp = self.format.bytes_per_pixel();
        let expected = (self.width as u64) * (self.height as u64) * (bpp as u64);
        if self.data.len() as u64 != expected {
            return Err(OcrError::InvalidImage {
                message: format!(
                    "expected {expected} bytes for {}x{} {:?} image, got {}",
                    self.width,
                    self.height,
                    self.format,
                    self.data.len()
                ),
            });
        }
        if self.width > i32::MAX as u32 || self.height > i32::MAX as u32 {
            return Err(OcrError::InvalidImage {
                message: "image too large for Tesseract".into(),
            });
        }
        let bytes_per_line = self
            .width
            .checked_mul(bpp)
            .filter(|&v| v <= i32::MAX as u32)
            .ok_or_else(|| OcrError::InvalidImage {
                message: "image too large for Tesseract".into(),
            })?;
        Ok((
            self.width as i32,
            self.height as i32,
            bpp as i32,
            bytes_per_line as i32,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_gray_buffer_size() {
        let err = OcrImage::new(&[0; 10], 4, 4, PixelFormat::Gray)
            .validate()
            .unwrap_err();
        assert!(matches!(err, OcrError::InvalidImage { .. }), "{err:?}");

        let (w, h, bpp, bpl) = OcrImage::new(&[0; 16], 4, 4, PixelFormat::Gray)
            .validate()
            .unwrap();
        assert_eq!((w, h, bpp, bpl), (4, 4, 1, 4));
    }

    #[test]
    fn validates_rgb_buffer_size() {
        let (w, h, bpp, bpl) = OcrImage::new(&[0; 48], 4, 4, PixelFormat::Rgb)
            .validate()
            .unwrap();
        assert_eq!((w, h, bpp, bpl), (4, 4, 3, 12));
    }

    #[test]
    fn rejects_zero_dimensions() {
        let err = OcrImage::new(&[], 0, 0, PixelFormat::Gray)
            .validate()
            .unwrap_err();
        assert!(matches!(err, OcrError::InvalidImage { .. }));
    }

    #[test]
    fn rejects_zero_dpi() {
        let err = OcrImage::new(&[0; 4], 2, 2, PixelFormat::Gray)
            .with_dpi(0)
            .validate()
            .unwrap_err();
        assert!(matches!(err, OcrError::InvalidImage { .. }));
    }

    #[test]
    fn default_dpi_is_300() {
        let image = OcrImage::new(&[0; 4], 2, 2, PixelFormat::Gray);
        assert_eq!(image.dpi(), 300);
        assert_eq!(image.with_dpi(150).dpi(), 150);
    }

    #[test]
    fn bytes_per_pixel() {
        assert_eq!(PixelFormat::Gray.bytes_per_pixel(), 1);
        assert_eq!(PixelFormat::Rgb.bytes_per_pixel(), 3);
        assert_eq!(PixelFormat::Rgba.bytes_per_pixel(), 4);
    }
}
