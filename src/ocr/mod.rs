//! Tesseract 5 OCR: a runtime-loaded binding to `libtesseract`.
//!
//! # Binding choice
//!
//! chilli-pdf must load Tesseract from the *extracted asset directory* at
//! runtime (issue #9 ships `libtesseract` next to the binary); it cannot rely
//! on a system-wide install. That rules out the usual binding crates:
//!
//! * the `tesseract` and `leptess` crates link against a *build-time*
//!   `libtesseract` discovered on the build machine. The final binary then has
//!   a hard dependency on whatever path the linker baked in, which defeats
//!   asset bundling and breaks fully-offline use;
//! * both also pull in far more surface than this crate needs (leptonica
//!   handles, config plumbing).
//!
//! So this module hand-rolls a *minimal* binding to the flat, stable C API
//! (`TessBaseAPI*`) instead, loaded with [`libloading`] (a tiny,
//! dependency-free wrapper over `dlopen`/`LoadLibraryW`):
//!
//! * the library is opened from an absolute path supplied by an injectable
//!   [`OcrPaths`] resolver — the extracted assets in production, the system
//!   package manager in development ([`SystemOcrPaths`]);
//! * only 17 flat C functions are needed, and none require binding leptonica:
//!   `TessBaseAPISetImage` takes raw pixel bytes directly;
//! * machines without a native Tesseract can still build and link this crate
//!   — resolution happens at runtime, and tests skip gracefully.
//!
//! # Privacy
//!
//! [`OcrError`] messages are content-free by construction: they carry paths,
//! language codes and image dimensions, never recognised text or image pixels,
//! so they are safe to display or log under the project's privacy spec.
//!
//! # Threading
//!
//! A Tesseract handle is not thread-safe. [`OcrEngine`] owns raw pointers, so
//! it is neither [`Send`] nor [`Sync`]; recognise from the thread that created
//! the engine (the conversion worker owns one engine for the whole job).
//!
//! [`libloading`]: libloading

mod error;
mod ffi;
mod image;
mod paths;
mod types;

pub use error::OcrError;
pub use image::{DEFAULT_DPI, OcrImage, PixelFormat};
pub use paths::{OcrPaths, StaticOcrPaths, SystemOcrPaths};
pub use types::{OcrLine, OcrPage, OcrWord, Rect};

use crate::language::LanguagePreset;

/// A Tesseract engine initialised for one language preset.
///
/// Create one with [`OcrEngine::new`], then feed it pages with
/// [`OcrEngine::recognize_page`]. The engine is reusable across pages and
/// clears Tesseract's state between recognitions.
pub struct OcrEngine {
    api: ffi::TesseractApi,
    preset: LanguagePreset,
}

impl OcrEngine {
    /// Load libtesseract, point it at the traineddata resolved by `paths`, and
    /// initialise recognition for `preset`'s language codes.
    pub fn new(preset: LanguagePreset, paths: &dyn OcrPaths) -> Result<Self, OcrError> {
        // Resolve tessdata and confirm the traineddata files exist before
        // touching the native library: a missing-model error is far clearer
        // than a opaque Tesseract init failure, and it lets tests assert the
        // error without a native Tesseract present.
        let tessdata = paths.tessdata_dir()?;
        for code in preset.tessdata_codes() {
            if !tessdata.join(format!("{code}.traineddata")).is_file() {
                return Err(OcrError::MissingTraineddata {
                    code,
                    dir: tessdata.clone(),
                });
            }
        }
        let libraries = paths.library_paths()?;
        let api = ffi::TesseractApi::open(&libraries, &tessdata, &language_string(preset))?;
        Ok(Self { api, preset })
    }

    /// The preset this engine was initialised for.
    pub const fn preset(&self) -> LanguagePreset {
        self.preset
    }

    /// Recognise one page image, returning structured per-word results grouped
    /// by line.
    pub fn recognize_page(&mut self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError> {
        let (width, height, bpp, bytes_per_line) = image.validate()?;
        self.api
            .set_image(image.data(), width, height, bpp, bytes_per_line);
        self.api.set_source_resolution(image.dpi() as i32);

        let result = if self.api.recognize() != 0 {
            Err(OcrError::RecognitionFailed {
                language: self.preset.id().to_owned(),
            })
        } else {
            self.api
                .iterator()
                .map(|it| collect_page(&it, image.width(), image.height(), image.dpi()))
                .unwrap_or_else(|| Ok(OcrPage::empty(image.width(), image.height(), image.dpi())))
        };

        // Reset Tesseract's image state so the next call starts clean.
        self.api.clear();
        result
    }
}

/// Join a preset's tessdata codes into Tesseract's `eng+hun` language string.
fn language_string(preset: LanguagePreset) -> String {
    preset.tessdata_codes().join("+")
}

/// A line under construction during [`collect_page`].
struct PendingLine {
    rect: Option<Rect>,
    words: Vec<OcrWord>,
}

impl PendingLine {
    fn finish(self) -> OcrLine {
        let rect = self.rect.unwrap_or_else(|| union_of(&self.words));
        OcrLine {
            rect,
            words: self.words,
        }
    }
}

/// Bounding box that encloses every word (fallback when Tesseract reports no
/// line-level box).
fn union_of(words: &[OcrWord]) -> Rect {
    let mut rect = Rect::default();
    for (i, word) in words.iter().enumerate() {
        if i == 0 {
            rect = word.rect;
        } else {
            rect.left = rect.left.min(word.rect.left);
            rect.top = rect.top.min(word.rect.top);
            rect.right = rect.right.max(word.rect.right);
            rect.bottom = rect.bottom.max(word.rect.bottom);
        }
    }
    rect
}

/// Walk the result iterator, grouping words into lines.
fn collect_page(
    iterator: &ffi::ResultIterator<'_>,
    width: u32,
    height: u32,
    dpi: u32,
) -> Result<OcrPage, OcrError> {
    let mut lines: Vec<OcrLine> = Vec::new();
    let mut current: Option<PendingLine> = None;

    loop {
        if current.is_some() && iterator.starts_new_line() {
            lines.push(current.take().expect("line is present").finish());
        }

        let text = iterator.word_text()?;
        if !text.is_empty() {
            let word_rect = iterator.word_box().unwrap_or_default();
            let pending = current.get_or_insert_with(|| PendingLine {
                rect: iterator.line_box(),
                words: Vec::new(),
            });
            pending.words.push(OcrWord {
                text,
                confidence: iterator.word_confidence(),
                rect: word_rect,
            });
        }

        if !iterator.next_word() {
            break;
        }
    }

    if let Some(line) = current.take() {
        lines.push(line.finish());
    }

    Ok(OcrPage {
        width,
        height,
        dpi,
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("chilli-ocr-test-{}-{tag}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn language_string_joins_codes() {
        assert_eq!(language_string(LanguagePreset::English), "eng");
        assert_eq!(language_string(LanguagePreset::Hungarian), "hun");
        assert_eq!(language_string(LanguagePreset::EnglishHungarian), "eng+hun");
    }

    #[test]
    fn missing_traineddata_reported_before_library_load() {
        // A tessdata dir that exists but holds no traineddata files, paired
        // with a bogus library: the traineddata check fires first, so this
        // test passes without a native Tesseract installed.
        let dir = scratch_dir("no-traineddata");
        let paths = StaticOcrPaths::new(
            vec![PathBuf::from("/nonexistent/chilli-pdf-libtesseract.so")],
            &dir,
        );
        let err = OcrEngine::new(LanguagePreset::Hungarian, &paths)
            .err()
            .expect("missing traineddata must fail initialisation");
        assert!(
            matches!(err, OcrError::MissingTraineddata { code: "hun", .. }),
            "{err:?}"
        );
    }

    #[test]
    fn missing_library_reported_after_traineddata_check() {
        let dir = scratch_dir("no-library");
        fs::write(dir.join("hun.traineddata"), b"stub").unwrap();
        fs::write(dir.join("eng.traineddata"), b"stub").unwrap();
        let paths = StaticOcrPaths::new(
            vec![PathBuf::from("/nonexistent/chilli-pdf-libtesseract.so")],
            &dir,
        );
        let err = OcrEngine::new(LanguagePreset::EnglishHungarian, &paths)
            .err()
            .expect("missing library must fail initialisation");
        assert!(
            matches!(
                err,
                OcrError::LibraryLoad { .. } | OcrError::LibraryNotFound { .. }
            ),
            "{err:?}"
        );
    }
}
