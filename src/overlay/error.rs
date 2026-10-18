//! Error type for the invisible-text overlay writer.
//!
//! Messages never include document contents — only paths and page numbers —
//! per the privacy spec in `docs/product-requirements.md`.

use std::fmt;
use std::path::PathBuf;

/// Everything that can go wrong while writing an invisible-text overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayError {
    /// The input PDF could not be read (missing, unreadable, or malformed).
    ReadFailed {
        /// The path that was attempted.
        path: PathBuf,
    },
    /// The input PDF is encrypted; object-level overlay editing is not
    /// supported for encrypted documents.
    EncryptedUnsupported {
        /// The path that was attempted.
        path: PathBuf,
    },
    /// The caller supplied a different number of OCR pages than the document
    /// has pages.
    PageCountMismatch {
        /// The input path, for context.
        path: PathBuf,
        /// The number of OCR results supplied by the caller.
        provided: usize,
        /// The number of pages actually in the document.
        in_document: usize,
    },
    /// A sparse overlay entry named a page that the document does not have.
    PageIndexOutOfRange {
        /// The offending page index, zero-based (as supplied by the caller).
        page: usize,
        /// The number of pages actually in the document.
        in_document: usize,
    },
    /// A page's geometry (crop box) is degenerate or non-finite, so no
    /// display-space position can be derived from it.
    InvalidPageGeometry {
        /// The page index, zero-based (as used in the API).
        page: usize,
    },
    /// The OCR result's pixel dimensions do not match the page geometry at
    /// the reported DPI — the OCR result and metadata are mispaired.
    OcrGeometryMismatch {
        /// The page index, zero-based (as used in the API).
        page: usize,
    },
    /// The injected font bytes are not a usable TrueType/OpenType font.
    InvalidFont,
    /// The page object could not be updated with the overlay (unexpected
    /// object shapes — the input is not a conforming PDF page).
    ContentUpdateFailed {
        /// The page index, zero-based (as used in the API).
        page: usize,
    },
    /// Writing the merged document to the output path failed.
    WriteFailed {
        /// The path that was attempted.
        path: PathBuf,
    },
    /// Serializing the merged document to the output stream failed (the
    /// caller-owned sink rejected the bytes, e.g. a full disk).
    SerializeFailed,
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed { path } => {
                write!(f, "the PDF at {} could not be read", path.display())
            }
            Self::EncryptedUnsupported { path } => {
                write!(
                    f,
                    "the PDF at {} is encrypted, which the overlay writer does not support",
                    path.display()
                )
            }
            Self::PageCountMismatch {
                path,
                provided,
                in_document,
            } => {
                write!(
                    f,
                    "the PDF at {} has {} page{} but {} OCR result{} were supplied",
                    path.display(),
                    in_document,
                    if *in_document == 1 { "" } else { "s" },
                    provided,
                    if *provided == 1 { "" } else { "s" },
                )
            }
            Self::PageIndexOutOfRange { page, in_document } => {
                write!(
                    f,
                    "the overlay results reference page {} but the document only has {} page{}",
                    page + 1,
                    in_document,
                    if *in_document == 1 { "" } else { "s" },
                )
            }
            Self::InvalidPageGeometry { page } => {
                write!(f, "page {} has an invalid size or crop box", page + 1)
            }
            Self::OcrGeometryMismatch { page } => {
                write!(
                    f,
                    "page {} has OCR results whose pixel size does not match the page geometry",
                    page + 1
                )
            }
            Self::InvalidFont => {
                write!(f, "the supplied overlay font is not a usable TrueType font")
            }
            Self::ContentUpdateFailed { page } => {
                write!(f, "page {} could not be updated with the overlay", page + 1)
            }
            Self::WriteFailed { path } => {
                write!(f, "the PDF could not be written to {}", path.display())
            }
            Self::SerializeFailed => {
                write!(
                    f,
                    "the searchable PDF could not be written to the output stream"
                )
            }
        }
    }
}

impl std::error::Error for OverlayError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths are fine in messages (they are not document contents); page
    /// numbers are phrased one-based.
    #[test]
    fn error_messages_never_leak_document_contents() {
        let e = OverlayError::ReadFailed {
            path: PathBuf::from("/tmp/secret.pdf"),
        };
        assert_eq!(
            e.to_string(),
            "the PDF at /tmp/secret.pdf could not be read"
        );

        let e = OverlayError::OcrGeometryMismatch { page: 2 };
        assert_eq!(
            e.to_string(),
            "page 3 has OCR results whose pixel size does not match the page geometry"
        );
    }

    #[test]
    fn page_count_mismatch_message() {
        let e = OverlayError::PageCountMismatch {
            path: PathBuf::from("in.pdf"),
            provided: 2,
            in_document: 1,
        };
        assert_eq!(
            e.to_string(),
            "the PDF at in.pdf has 1 page but 2 OCR results were supplied"
        );
    }
}
