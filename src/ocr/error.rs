//! Errors returned by the OCR engine.

use std::fmt;
use std::path::PathBuf;

/// Something went wrong locating the native OCR runtime, initialising
/// Tesseract, or recognising a page.
///
/// Error messages are **content-free by construction**: they carry paths,
/// language codes and image dimensions, but never recognised text or image
/// pixels, so they are safe to display and log under the project's privacy
/// spec.
#[derive(Debug)]
pub enum OcrError {
    /// No native Tesseract library could be located.
    ///
    /// `searched` lists the places that were probed, for troubleshooting.
    LibraryNotFound { searched: Vec<PathBuf> },
    /// The native library was found but could not be loaded.
    LibraryLoad { path: PathBuf, message: String },
    /// The loaded library is missing a required C API symbol.
    MissingSymbol { symbol: &'static str },
    /// No tessdata directory could be located.
    TessdataNotFound { searched: Vec<PathBuf> },
    /// The tessdata directory does not contain the traineddata for a language.
    MissingTraineddata { code: &'static str, dir: PathBuf },
    /// A path or parameter string contained an interior NUL byte.
    InvalidPath { value: String },
    /// Tesseract could not be initialised for the requested language.
    InitialisationFailed { language: String },
    /// The supplied image buffer does not match its declared dimensions.
    InvalidImage { message: String },
    /// Tesseract failed while recognising the page.
    RecognitionFailed { language: String },
    /// Tesseract returned text that was not valid UTF-8.
    InvalidText,
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound { searched } => {
                write!(
                    f,
                    "no Tesseract library found (searched {} location",
                    searched.len()
                )?;
                if searched.len() != 1 {
                    f.write_str("s")?;
                }
                if !searched.is_empty() {
                    write!(f, ": {}", display_paths(searched))?;
                }
                f.write_str(")")
            }
            Self::LibraryLoad { path, message } => {
                write!(
                    f,
                    "failed to load Tesseract library {}: {}",
                    path.display(),
                    message
                )
            }
            Self::MissingSymbol { symbol } => {
                write!(f, "Tesseract library is missing the `{symbol}` C symbol")
            }
            Self::TessdataNotFound { searched } => {
                write!(
                    f,
                    "no tessdata directory found (searched {} location",
                    searched.len()
                )?;
                if searched.len() != 1 {
                    f.write_str("s")?;
                }
                if !searched.is_empty() {
                    write!(f, ": {}", display_paths(searched))?;
                }
                f.write_str(")")
            }
            Self::MissingTraineddata { code, dir } => {
                write!(f, "`{code}.traineddata` not found in {}", dir.display())
            }
            Self::InvalidPath { value } => {
                write!(f, "path contained an interior NUL byte: {value}")
            }
            Self::InitialisationFailed { language } => {
                write!(
                    f,
                    "Tesseract failed to initialise for language `{language}`"
                )
            }
            Self::InvalidImage { message } => write!(f, "invalid image: {message}"),
            Self::RecognitionFailed { language } => {
                write!(
                    f,
                    "Tesseract failed to recognise a page for language `{language}`"
                )
            }
            Self::InvalidText => f.write_str("Tesseract returned text that is not valid UTF-8"),
        }
    }
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl std::error::Error for OcrError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_render_without_panicking() {
        let cases: Vec<OcrError> = vec![
            OcrError::LibraryNotFound { searched: vec![] },
            OcrError::LibraryNotFound {
                searched: vec!["/a".into(), "/b".into()],
            },
            OcrError::LibraryLoad {
                path: "/x.so".into(),
                message: "boom".into(),
            },
            OcrError::MissingSymbol {
                symbol: "TessBaseAPICreate",
            },
            OcrError::TessdataNotFound {
                searched: vec!["/t".into()],
            },
            OcrError::MissingTraineddata {
                code: "hun",
                dir: "/t".into(),
            },
            OcrError::InvalidPath { value: "/t".into() },
            OcrError::InitialisationFailed {
                language: "eng+hun".into(),
            },
            OcrError::InvalidImage {
                message: "bad".into(),
            },
            OcrError::RecognitionFailed {
                language: "eng".into(),
            },
            OcrError::InvalidText,
        ];
        for err in cases {
            let text = err.to_string();
            assert!(!text.is_empty());
        }
    }

    /// Content-freedom pin for the OCR failure paths (issue #20 audit).
    ///
    /// The privacy spec forbids document contents in any message.
    /// `OcrError` has no variant that can carry recognized text or image
    /// pixels: every field is a path, language code, symbol name,
    /// OS/library-load message or image dimension. These exact-string
    /// assertions pin the `Display` format functions for the failure
    /// paths, so a future variant that interpolates OCR output must
    /// change this test to be noticed.
    #[test]
    fn error_messages_are_content_free() {
        // The recognition failure path (Tesseract fails mid-page):
        // only the language code is interpolated, never page text.
        assert_eq!(
            OcrError::RecognitionFailed {
                language: "eng+hun".into()
            }
            .to_string(),
            "Tesseract failed to recognise a page for language `eng+hun`"
        );
        // Initialisation failure: language code only.
        assert_eq!(
            OcrError::InitialisationFailed {
                language: "eng".into()
            }
            .to_string(),
            "Tesseract failed to initialise for language `eng`"
        );
        // Recognised text that is not valid UTF-8 is described, never
        // quoted: the variant carries no payload at all.
        assert_eq!(
            OcrError::InvalidText.to_string(),
            "Tesseract returned text that is not valid UTF-8"
        );
        // Invalid images are described by dimensions/format only
        // (messages are built in `src/ocr/image.rs`).
        assert_eq!(
            OcrError::InvalidImage {
                message: "expected 120000 bytes for 100x100 RGBA image, got 3".into()
            }
            .to_string(),
            "invalid image: expected 120000 bytes for 100x100 RGBA image, got 3"
        );

        // Structural guarantee, checked by inspection of the enum:
        // no OcrError variant has a field that any production call
        // site fills with OCR output or image data — the fields are
        // paths (`LibraryNotFound`, `LibraryLoad`, `TessdataNotFound`,
        // `MissingTraineddata`, `InvalidPath`), language codes
        // (`MissingTraineddata`, `InitialisationFailed`,
        // `RecognitionFailed`), a C symbol name (`MissingSymbol`), a
        // dimension-only message built in `src/ocr/image.rs`
        // (`InvalidImage`), or nothing at all (`InvalidText`).
        // Recognized text flows through `OcrPage`, which never touches
        // an error value.
    }
}
