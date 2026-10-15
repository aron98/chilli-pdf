//! Core building blocks for chilli-pdf.
//!
//! The crate is intentionally dependency-free at this stage; the conversion
//! pipeline (PDFium rendering, Tesseract OCR, invisible-text overlay) lands
//! milestone by milestone — see the issue tracker. Everything in this crate
//! must compile and pass tests on Windows, macOS, and Linux.

pub mod assets;
pub mod language;
pub mod output_path;

/// Application version, mirrored from Cargo so the GUI and CLI agree.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
