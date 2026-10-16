//! Error type for PDF open, inspect, render, and save failures.
//!
//! Messages never include document contents — only page numbers and
//! error categories — per the privacy spec in `docs/product-requirements.md`.

use std::fmt;

/// Everything that can go wrong while opening, inspecting, rendering, or
/// saving a PDF.
///
/// Whether a given condition is *acceptable* (e.g. is a password-protected
/// file a hard error or a user prompt?) is policy, decided in
/// `crate::validation`; this type only names the condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfError {
    /// The PDFium library could not be found at the resolved path.
    LibraryUnavailable {
        /// The path that was searched.
        path: std::path::PathBuf,
    },
    /// The PDFium library was already bound in this process (allowed once).
    LibraryAlreadyBound,
    /// The PDFium library was found but could not be loaded.
    LibraryLoadFailed {
        /// The path that failed to load.
        path: std::path::PathBuf,
    },
    /// The file could not be read (missing, unreadable, or not a PDF at all).
    FileUnreadable {
        /// The path that was attempted.
        path: std::path::PathBuf,
    },
    /// The file is password-protected.
    PasswordProtected {
        /// The path that was attempted.
        path: std::path::PathBuf,
    },
    /// The file uses encryption not supported by the engine.
    EncryptionUnsupported {
        /// The path that was attempted.
        path: std::path::PathBuf,
    },
    /// The file exists but is not a well-formed PDF.
    Malformed {
        /// The path that was attempted.
        path: std::path::PathBuf,
    },
    /// The requested page index is outside the document's page range.
    PageOutOfBounds {
        /// The requested page index, zero-based (as used in the API).
        page: usize,
        /// The total number of pages in the document.
        page_count: usize,
    },
    /// A page's geometry (size or crop box) is degenerate or non-finite.
    InvalidPageGeometry {
        /// The page index, zero-based (as used in the API).
        page: usize,
    },
    /// The page is so large that even the minimum usable DPI exceeds the
    /// rendering pixel budget; rendering it would risk exhausting memory.
    PageTooLarge {
        /// The page index, zero-based (as used in the API).
        page: usize,
    },
    /// PDFium refused to render the page.
    RenderFailed {
        /// The page index, zero-based (as used in the API).
        page: usize,
    },
    /// Writing the document to the destination path failed.
    SaveFailed {
        /// The path that was attempted.
        path: std::path::PathBuf,
    },
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryUnavailable { path } => {
                write!(f, "the PDFium library was not found at {}", path.display())
            }
            Self::LibraryAlreadyBound => {
                write!(f, "the PDFium library is already loaded in this process")
            }
            Self::LibraryLoadFailed { path } => {
                write!(
                    f,
                    "the PDFium library at {} could not be loaded",
                    path.display()
                )
            }
            Self::FileUnreadable { path } => {
                write!(
                    f,
                    "the file at {} could not be read as a PDF",
                    path.display()
                )
            }
            Self::PasswordProtected { path } => {
                write!(f, "the PDF at {} is password-protected", path.display())
            }
            Self::EncryptionUnsupported { path } => {
                write!(
                    f,
                    "the PDF at {} uses an unsupported form of encryption",
                    path.display()
                )
            }
            Self::Malformed { path } => {
                write!(f, "the file at {} is not a well-formed PDF", path.display())
            }
            Self::PageOutOfBounds { page, page_count } => {
                write!(
                    f,
                    "page {} was requested but the document only has {} page{}",
                    page + 1,
                    page_count,
                    if *page_count == 1 { "" } else { "s" },
                )
            }
            Self::InvalidPageGeometry { page } => {
                write!(f, "page {} has an invalid size or crop box", page + 1,)
            }
            Self::PageTooLarge { page } => {
                write!(
                    f,
                    "page {} is too large to render within the pixel budget",
                    page + 1,
                )
            }
            Self::RenderFailed { page } => {
                write!(f, "page {} could not be rendered", page + 1)
            }
            Self::SaveFailed { path } => {
                write!(f, "the PDF could not be saved to {}", path.display())
            }
        }
    }
}

impl std::error::Error for PdfError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path is fine to embed in messages (paths are not document contents).
    #[test]
    fn error_messages_never_leak_document_contents() {
        let e = PdfError::Malformed {
            path: std::path::PathBuf::from("/tmp/secret.pdf"),
        };
        assert_eq!(
            e.to_string(),
            "the file at /tmp/secret.pdf is not a well-formed PDF"
        );
    }

    #[test]
    fn page_errors_are_one_based_in_messages() {
        let e = PdfError::PageOutOfBounds {
            page: 2,
            page_count: 3,
        };
        assert_eq!(
            e.to_string(),
            "page 3 was requested but the document only has 3 pages"
        );

        let e = PdfError::PageTooLarge { page: 0 };
        assert_eq!(
            e.to_string(),
            "page 1 is too large to render within the pixel budget"
        );
    }

    #[test]
    fn single_page_uses_singular_word() {
        let e = PdfError::PageOutOfBounds {
            page: 1,
            page_count: 1,
        };
        assert_eq!(
            e.to_string(),
            "page 2 was requested but the document only has 1 page"
        );
    }
}
