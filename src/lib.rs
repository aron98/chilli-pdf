//! Core building blocks for chilli-pdf.
//!
//! The conversion pipeline (PDFium rendering, Tesseract OCR, invisible-text
//! overlay) lands milestone by milestone — see the issue tracker. The GUI
//! shell (`app`, behind the default-on `gui` feature) is a thin window over
//! the headless state machine in [`app_state`], which is plain Rust and
//! unit-testable without a display. Everything in this crate must compile
//! and pass tests on Windows, macOS, and Linux.

pub mod app_state;
pub mod converter;
pub mod language;
pub mod ocr;
pub mod output_path;

#[cfg(feature = "gui")]
pub mod app;

/// Application version, mirrored from Cargo so the GUI and CLI agree.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
