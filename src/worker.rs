//! Conversion worker: wires the existing stages into the end-to-end
//! pipeline behind the [`Converter`] contract (issue #10).
//!
//! Per document: pre-flight validation → open the document → for each
//! page: extract the existing text → skip-text classification → render at
//! the standard DPI → OCR → collect the page's words and geometry → after
//! the last page, merge the invisible text layer through the overlay seam
//! and publish transactionally.
//!
//! # Threading model
//!
//! The PDF and OCR engine traits are deliberately `!Send` (PDFium is not
//! thread-safe per document, and the native bindings are process-global).
//! [`PipelineConverter`] therefore owns a single long-lived *actor* thread
//! created at construction time. All engine work — and the one-per-process
//! PDFium engine — lives there for the converter's lifetime. The GUI-facing
//! [`Converter::start`] only spawns a thin waiter thread so
//! [`Worker::join`](crate::converter::Worker::join) keeps its "the run has
//! fully stopped" meaning. Jobs are naturally queued by the actor's
//! channel; the GUI runs one at a time.
//!
//! # Bounded memory
//!
//! The pipeline is strictly page-at-a-time: at most one rendered bitmap
//! and one page's OCR words are alive at any moment, and both are dropped
//! before the next page renders. The only cross-page accumulation is
//! `Vec<PageOcrResult>` — small text/geometry structs (a word string, a
//! confidence, a pixel rectangle and the page's metadata) — which the
//! overlay needs at merge time. That trade is deliberate: re-rendering
//! every page at merge time would double the expensive work, while the
//! accumulated structs stay in the kilobytes-per-page range.
//!
//! # Cooperative cancellation
//!
//! The [`CancelHandle`](crate::converter::CancelHandle) flag is polled
//! between stages and between pages. On cancel the run stops promptly,
//! the transactional writer removes any temp file, nothing is published,
//! and the GUI sees [`WorkerEvent::Cancelled`](crate::converter::WorkerEvent)
//! — distinct from a failure.

use std::fmt;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread;

use crate::assets::AssetError;
use crate::assets::ExtractedAssets;
use crate::assets::NativeLib;
use crate::assets::ensure_assets;
use crate::converter::CancelHandle;
use crate::converter::Converter;
use crate::converter::Job;
use crate::converter::Worker;
use crate::converter::WorkerEvent;
use crate::language::LanguagePreset;
use crate::ocr::OcrEngine;
use crate::ocr::OcrError;
use crate::ocr::OcrImage;
use crate::ocr::OcrPage;
use crate::ocr::OcrPaths;
use crate::ocr::PixelFormat;
use crate::output_path::Conflict;
use crate::output_path::check_output;
use crate::pdf::FixedLibraryPath;
use crate::pdf::PageMetadata;
use crate::pdf::PdfEngine;
use crate::pdf::PdfError;
use crate::pdf::PdfiumEngine;
use crate::pdf::RenderOptions;
use crate::skip_text::PageTextVerdict;
use crate::skip_text::SkipThresholds;
use crate::skip_text::classify_page;
use crate::transactional::DocumentWriter;
use crate::transactional::IfExists;
use crate::transactional::PublishError;
use crate::transactional::publish_document;
use crate::validation::ValidationConfig;
use crate::validation::ValidationError;
use crate::validation::validate_bytes;
use crate::validation::validate_size;

/// How a run ended, before it is turned into a [`WorkerEvent`].
#[derive(Debug)]
enum Outcome {
    /// The searchable document was published to the output path.
    Completed,
    /// The user cancelled; nothing was published.
    Cancelled,
    /// The UI dropped the event receiver mid-run; stop quietly without
    /// publishing (the same convention as the mock converter).
    Abandoned,
}

/// One page's OCR result, tagged with its zero-based page index and the
/// page geometry the overlay needs to map pixel coordinates back onto the
/// page ([`OcrPage`] itself carries no page number).
#[derive(Debug, Clone)]
pub struct PageOcrResult {
    /// Zero-based page index within the source document.
    pub page: usize,
    /// The words Tesseract found on the rendered page.
    pub ocr: OcrPage,
    /// Rotation and crop box of the page, captured before rendering.
    pub metadata: PageMetadata,
}

/// The OCR seam the pipeline needs from [`OcrEngine`].
///
/// Split out so the pipeline can be tested with fakes instead of the
/// native-library-backed engine, without changing the OCR module's API.
pub trait PageRecognizer {
    /// Recognise one rendered page. The image is borrowed from the
    /// caller's bitmap, which stays alive only for this call.
    fn recognize(&mut self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError>;
}

impl PageRecognizer for OcrEngine {
    fn recognize(&mut self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError> {
        self.recognize_page(image)
    }
}

/// Failure of the overlay seam.
#[derive(Debug)]
pub enum OverlayError {
    /// The overlay milestone (issue #13) has not been wired into the
    /// worker yet. Typed separately so the GUI can word the message
    /// honestly instead of showing a generic save failure.
    Pending,
    /// The overlay implementation reported a failure.
    Failed { message: String },
}

impl OverlayError {
    /// User-facing message; never contains document contents.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Pending => concat!(
                "Adding the searchable text layer is not wired up in this ",
                "build yet (overlay milestone, issue #13). Nothing was ",
                "written."
            )
            .to_owned(),
            Self::Failed { message } => {
                format!("Adding the searchable text layer failed: {message}. Nothing was written.")
            }
        }
    }
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for OverlayError {}

/// The overlay seam: merge an invisible text layer built from the OCR
/// results into the original document, streaming the finished PDF bytes
/// into `sink`.
///
/// The seam is sink-based (rather than writing to a path itself) so the
/// output goes through the transactional writer: whatever the overlay
/// streams lands in the temp file and only the completed document is
/// renamed into place. The real implementation is milestone #13; until it
/// merges the production converter uses [`UnimplementedOverlay`].
pub trait PageOverlay: Send + Sync {
    /// Read the original document at `input`, add the searchable text
    /// layer for `pages` (using `font` — the glyphless TrueType font — for
    /// the invisible text), and write the complete new PDF to `sink`.
    fn apply_overlay(
        &self,
        input: &Path,
        pages: &[PageOcrResult],
        font: &[u8],
        sink: &mut dyn Write,
    ) -> Result<(), OverlayError>;
}

/// Placeholder real-world overlay: every conversion reaches it and reports
/// the typed "pending milestone #13" failure, so the wiring end-to-end is
/// exercised until the overlay lands.
pub struct UnimplementedOverlay;

impl PageOverlay for UnimplementedOverlay {
    fn apply_overlay(
        &self,
        _input: &Path,
        _pages: &[PageOcrResult],
        _font: &[u8],
        _sink: &mut dyn Write,
    ) -> Result<(), OverlayError> {
        // TODO(#13): replace with an adapter over src/overlay's real
        // implementation once issue #13 merges.
        Err(OverlayError::Pending)
    }
}

/// Test double for [`PageOverlay`]: records each call and streams a fixed
/// marker document.
#[derive(Debug, Clone, Default)]
pub struct MockOverlay {
    calls: Arc<Mutex<Vec<OverlayCallRecord>>>,
}

/// One recorded [`MockOverlay`] invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayCallRecord {
    /// The input path the overlay was asked to read.
    pub input: PathBuf,
    /// Zero-based indices of the pages handed to the overlay.
    pub pages: Vec<usize>,
    /// The font bytes handed to the overlay.
    pub font: Vec<u8>,
}

impl MockOverlay {
    /// Every recorded call, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<OverlayCallRecord> {
        self.calls
            .lock()
            .expect("overlay call log poisoned")
            .clone()
    }
}

impl PageOverlay for MockOverlay {
    fn apply_overlay(
        &self,
        input: &Path,
        pages: &[PageOcrResult],
        font: &[u8],
        sink: &mut dyn Write,
    ) -> Result<(), OverlayError> {
        self.calls
            .lock()
            .expect("overlay call log poisoned")
            .push(OverlayCallRecord {
                input: input.to_path_buf(),
                pages: pages.iter().map(|result| result.page).collect(),
                font: font.to_vec(),
            });
        sink.write_all(b"%PDF-mock-searchable\n")
            .map_err(|err| OverlayError::Failed {
                message: err.to_string(),
            })
    }
}

/// Typed failure of a conversion run. Module errors are mapped here at
/// the boundary; [`ConversionError::user_message`] produces the
/// user-facing text and neither the messages nor the error values ever
/// contain document contents.
#[derive(Debug)]
pub enum ConversionError {
    /// The chosen output is the input document; never allowed.
    SameAsInput,
    /// The input file could not be read (missing, permission, …).
    InputUnreadable,
    /// Pre-flight validation rejected the document.
    Validation(ValidationError),
    /// The document could not be opened, or the PDF engine failed to start.
    Open(PdfError),
    /// A per-page PDF operation failed.
    Page(PdfError),
    /// Text recognition failed.
    Ocr(OcrError),
    /// The bundled native components could not be prepared.
    Assets(AssetError),
    /// The overlay seam reported milestone #13 pending.
    OverlayPending,
    /// The overlay seam failed.
    Overlay(OverlayError),
    /// The transactional publish failed.
    Publish(PublishError),
}

impl ConversionError {
    /// User-facing message; never contains document contents.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::SameAsInput => PublishError::Conflict(Conflict::SameAsInput).user_message(),
            Self::InputUnreadable => concat!(
                "The input file could not be read. Check that the file ",
                "still exists and can be opened, then try again."
            )
            .to_owned(),
            Self::Validation(err) => err.user_message(),
            Self::Open(err) => match err {
                PdfError::PasswordProtected { .. } | PdfError::EncryptionUnsupported { .. } => {
                    ValidationError::Encrypted.user_message()
                }
                PdfError::FileUnreadable { .. } | PdfError::Malformed { .. } => {
                    ValidationError::Malformed.user_message()
                }
                PdfError::LibraryUnavailable { .. }
                | PdfError::LibraryLoadFailed { .. }
                | PdfError::LibraryAlreadyBound => concat!(
                    "The PDF component could not be started. Reinstall ",
                    "chilli-pdf and try again."
                )
                .to_owned(),
                _ => format!("The document could not be processed: {err}"),
            },
            Self::Page(err) => format!("The document could not be processed: {err}"),
            Self::Ocr(err) => format!("Text recognition failed: {err}"),
            Self::Assets(err) => {
                format!("The bundled components could not be prepared on this computer: {err}")
            }
            Self::OverlayPending => OverlayError::Pending.user_message(),
            Self::Overlay(err) => err.user_message(),
            Self::Publish(err) => err.user_message(),
        }
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for ConversionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(err) => Some(err),
            Self::Open(err) | Self::Page(err) => Some(err),
            Self::Ocr(err) => Some(err),
            Self::Assets(err) => Some(err),
            Self::Overlay(err) => Some(err),
            Self::Publish(err) => Some(err),
            Self::SameAsInput | Self::InputUnreadable | Self::OverlayPending => None,
        }
    }
}

impl From<PdfError> for ConversionError {
    fn from(err: PdfError) -> Self {
        Self::Open(err)
    }
}

impl From<OcrError> for ConversionError {
    fn from(err: OcrError) -> Self {
        Self::Ocr(err)
    }
}

impl From<AssetError> for ConversionError {
    fn from(err: AssetError) -> Self {
        Self::Assets(err)
    }
}

/// How the actor builds its single PDF engine (lazily, once per process).
pub type PdfFactory = Arc<dyn Fn() -> Result<Box<dyn PdfEngine>, ConversionError> + Send + Sync>;

/// How the actor builds the per-document OCR engine.
pub type OcrFactory =
    Arc<dyn Fn(LanguagePreset) -> Result<Box<dyn PageRecognizer>, ConversionError> + Send + Sync>;

/// The real conversion worker (issue #10), satisfying the GUI's
/// [`Converter`] contract.
///
/// The heavy engine state lives on the converter's actor thread (see the
/// module docs); the handle itself only carries the job channel, which is
/// why it is `Send + Sync` as the trait requires.
pub struct PipelineConverter {
    requests: Sender<JobRequest>,
}

impl PipelineConverter {
    /// Build a converter from injectable seams: a lazily-created PDF
    /// engine, a per-document OCR engine factory, the overlay, and the
    /// overlay's font bytes.
    #[must_use]
    pub fn new(
        pdf_factory: PdfFactory,
        ocr_factory: OcrFactory,
        overlay: Arc<dyn PageOverlay>,
        font: Vec<u8>,
    ) -> Self {
        let (requests, inbox) = mpsc::channel::<JobRequest>();
        let actor_thread = thread::Builder::new()
            .name("chilli-pdf-pipeline".to_owned())
            .spawn(move || {
                // The engine state is born on the actor thread itself:
                // the PDF engine traits are deliberately !Send, so the
                // actor is constructed after the thread boundary from
                // its Send + Sync factories only.
                Actor {
                    pdf_factory,
                    ocr_factory,
                    overlay,
                    font,
                    pdf_engine: None,
                }
                .serve(inbox)
            })
            .expect("spawn pipeline actor thread");
        // The actor runs until the converter (and any in-flight waiter)
        // is dropped and the channel closes.
        drop(actor_thread);
        Self { requests }
    }

    /// The production converter: bundled PDFium and Tesseract via the
    /// extracted assets, and the placeholder overlay until milestone #13.
    ///
    /// Construction cannot fail: the engines are created lazily on the
    /// actor thread at the first job (and retried on later jobs if a
    /// component fails to start, e.g. because assets are still fixture
    /// bytes in development builds).
    #[must_use]
    pub fn production() -> Self {
        Self::new(
            Arc::new(production_pdf_engine),
            Arc::new(production_ocr_engine),
            // TODO(#13): replace with an adapter over src/overlay's real
            // implementation once it merges, and bundle the glyphless font
            // (pdf.ttf) in the asset manifest so `font` can be its bytes.
            Arc::new(UnimplementedOverlay),
            Vec::new(),
        )
    }
}

/// One queued job, as the waiter thread hands it to the actor.
struct JobRequest {
    job: Job,
    cancel: CancelHandle,
    events: Sender<WorkerEvent>,
    done: mpsc::Sender<()>,
}

impl Converter for PipelineConverter {
    fn start(self: Arc<Self>, job: Job, events: Sender<WorkerEvent>) -> Worker {
        let cancel = CancelHandle::fresh();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let requests = self.requests.clone();
        let thread_cancel = cancel.clone();
        let handle = thread::Builder::new()
            .name("chilli-pdf-converter".to_owned())
            .spawn(move || {
                let request = JobRequest {
                    job,
                    cancel: thread_cancel,
                    events,
                    done: done_tx,
                };
                if requests.send(request).is_err() {
                    // The actor is gone; nothing more to do.
                    return;
                }
                // Block until the actor reports the job finished, so
                // Worker::join() keeps its "the run has fully stopped"
                // meaning.
                let _ = done_rx.recv();
            })
            .expect("spawn converter waiter thread");
        Worker::new(cancel, Some(handle))
    }
}

/// The converter's engine thread: owns the PDF engine for the process
/// lifetime and runs one job at a time.
struct Actor {
    pdf_factory: PdfFactory,
    ocr_factory: OcrFactory,
    overlay: Arc<dyn PageOverlay>,
    font: Vec<u8>,
    /// Created once per process on this thread: PDFium's bindings cannot
    /// be initialised twice, and the engine traits are deliberately
    /// `!Send`.
    pdf_engine: Option<Box<dyn PdfEngine>>,
}

impl Actor {
    fn serve(mut self, requests: Receiver<JobRequest>) {
        while let Ok(request) = requests.recv() {
            self.handle(request);
        }
    }

    fn handle(&mut self, request: JobRequest) {
        // The PDF engine is created lazily and kept for the converter's
        // lifetime; if creation fails the job fails, but a later job
        // retries (e.g. after assets were installed).
        if self.pdf_engine.is_none() {
            match (self.pdf_factory)() {
                Ok(engine) => self.pdf_engine = Some(engine),
                Err(err) => {
                    let _ = request.events.send(WorkerEvent::Failed(err.user_message()));
                    let _ = request.done.send(());
                    return;
                }
            }
        }
        let Some(engine) = self.pdf_engine.as_deref() else {
            unreachable!("the PDF engine was just initialised");
        };
        let outcome = convert_document(
            &request.job,
            engine,
            &self.ocr_factory,
            self.overlay.as_ref(),
            &self.font,
            &request.cancel,
            &request.events,
        );
        let _ = match outcome {
            Ok(Outcome::Completed) => request.events.send(WorkerEvent::Done),
            Ok(Outcome::Cancelled) => request.events.send(WorkerEvent::Cancelled),
            Ok(Outcome::Abandoned) => Ok(()),
            Err(err) => request.events.send(WorkerEvent::Failed(err.user_message())),
        };
        let _ = request.done.send(());
    }
}

/// The document pipeline, run synchronously on the actor thread.
///
/// Progress: exactly one [`WorkerEvent::Progress`] per page (one-based
/// for the UI), sent after the page is classified/rendered/recognised —
/// never more than one message per page.
///
/// Bounded memory: see the module docs — one bitmap and one page's words
/// alive at a time.
#[allow(clippy::too_many_arguments)]
fn convert_document(
    job: &Job,
    engine: &dyn PdfEngine,
    ocr_factory: &OcrFactory,
    overlay: &dyn PageOverlay,
    font: &[u8],
    cancel: &CancelHandle,
    events: &Sender<WorkerEvent>,
) -> Result<Outcome, ConversionError> {
    // The GUI's ask-before-replace prompt has already run by the time a
    // job reaches the worker, so an existing output is replaced. The one
    // conflict that stays fatal is the original file as the destination.
    if let Err(Conflict::SameAsInput) = check_output(&job.input, &job.output) {
        return Err(ConversionError::SameAsInput);
    }

    // Pre-flight validation runs before any PDF engine touches the file:
    // encrypted, signed and malformed documents are rejected up front,
    // and oversize inputs are refused without ever reading them.
    let config = ValidationConfig::default();
    let metadata = fs::metadata(&job.input).map_err(|_| ConversionError::InputUnreadable)?;
    validate_size(metadata.len(), &config).map_err(ConversionError::Validation)?;
    let bytes = fs::read(&job.input).map_err(|_| ConversionError::InputUnreadable)?;
    validate_bytes(&bytes, &config).map_err(ConversionError::Validation)?;
    drop(bytes); // Bounded by the size cap; freed before the PDF work starts.

    if cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }

    // One document handle for the whole job: PDFium is not thread-safe
    // per document, and page handles borrow from it.
    let document = engine
        .open_document(&job.input)
        .map_err(ConversionError::Open)?;
    let total_pages = document.page_count().map_err(ConversionError::Open)?;

    let mut results = Vec::new();
    let mut recognizer: Option<Box<dyn PageRecognizer>> = None;

    for page in 0..total_pages {
        // Cooperative cancel: checked between pages, before new work.
        if cancel.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }

        // Pages whose existing text is already searchable are left alone;
        // a page whose text layer cannot be extracted is treated
        // conservatively as needing OCR rather than aborting the run.
        let verdict = match document.page_text(page) {
            Ok(text) => classify_page(&text, &SkipThresholds::default()),
            Err(_) => PageTextVerdict::NeedsOcr,
        };

        if verdict.needs_ocr() {
            let page_metadata = document
                .page_metadata(page)
                .map_err(ConversionError::Page)?;
            let bitmap = document
                .render_page(page, RenderOptions::default())
                .map_err(ConversionError::Page)?;
            let image = OcrImage::new(
                &bitmap.pixels,
                bitmap.width,
                bitmap.height,
                PixelFormat::Rgb,
            )
            .with_dpi(bitmap.dpi.round().max(1.0) as u32);

            // One Tesseract instance per document, created lazily on the
            // first page that needs it: fully digital documents never
            // load the OCR engine at all.
            if recognizer.is_none() {
                recognizer = Some(ocr_factory(job.language)?);
            }
            let Some(ocr_engine) = recognizer.as_deref_mut() else {
                unreachable!("the OCR engine was just initialised");
            };
            let page_ocr = ocr_engine.recognize(&image).map_err(ConversionError::Ocr)?;
            results.push(PageOcrResult {
                page,
                ocr: page_ocr,
                metadata: page_metadata,
            });
            // `bitmap` and `image` (a borrow of its pixels) end their
            // scope here: the next page renders only after this page's
            // pixels are dropped.
        }

        // Exactly one progress message per page, one-based for the UI.
        if events
            .send(WorkerEvent::Progress {
                page: page + 1,
                total_pages,
            })
            .is_err()
        {
            return Ok(Outcome::Abandoned);
        }
    }

    if cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }

    // Merge the searchable text layer into the original and publish it
    // transactionally: the overlay streams into the temp file and the
    // finished document is renamed into place.
    let mut writer = OverlayWriter {
        overlay,
        input: &job.input,
        pages: &results,
        font,
        error: None,
    };
    match publish_document(&job.input, &job.output, &mut writer, IfExists::Replace) {
        Ok(()) => Ok(Outcome::Completed),
        Err(err) => {
            // The transactional writer removes any temp file on failure,
            // so a cancellation observed this late is still a clean
            // stop with nothing published.
            if cancel.is_cancelled() {
                return Ok(Outcome::Cancelled);
            }
            match writer.error {
                Some(OverlayError::Pending) => Err(ConversionError::OverlayPending),
                Some(err) => Err(ConversionError::Overlay(err)),
                None => Err(ConversionError::Publish(err)),
            }
        }
    }
}

/// [`DocumentWriter`] adapter: streams the overlay seam's output into the
/// transactional writer's temp file.
struct OverlayWriter<'a> {
    overlay: &'a dyn PageOverlay,
    input: &'a Path,
    pages: &'a [PageOcrResult],
    font: &'a [u8],
    error: Option<OverlayError>,
}

impl DocumentWriter for OverlayWriter<'_> {
    fn write_document(&mut self, sink: &mut dyn Write) -> io::Result<()> {
        match self
            .overlay
            .apply_overlay(self.input, self.pages, self.font, sink)
        {
            Ok(()) => Ok(()),
            Err(err) => {
                self.error = Some(err);
                Err(io::Error::other("overlay failed"))
            }
        }
    }
}

/// Production PDF engine: bundled PDFium, loaded by absolute path from
/// the extracted assets. Called at most once per process on the actor
/// thread.
fn production_pdf_engine() -> Result<Box<dyn PdfEngine>, ConversionError> {
    let assets = ensure_assets()?;
    let source = FixedLibraryPath::new(assets.library_path(NativeLib::Pdfium)?);
    Ok(Box::new(PdfiumEngine::new(&source)?))
}

/// Production OCR engine: bundled Tesseract, one instance per document
/// (the spike's performance notes recommend reuse within a run; the
/// per-job engine is created lazily on the first page that needs it).
fn production_ocr_engine(
    preset: LanguagePreset,
) -> Result<Box<dyn PageRecognizer>, ConversionError> {
    let assets = ensure_assets()?;
    Ok(Box::new(OcrEngine::new(preset, &AssetOcrPaths { assets })?))
}

/// Adapter: the asset bundle as an [`OcrPaths`] source.
struct AssetOcrPaths {
    assets: ExtractedAssets,
}

impl OcrPaths for AssetOcrPaths {
    fn library_paths(&self) -> Result<Vec<PathBuf>, OcrError> {
        let path = self
            .assets
            .library_path(NativeLib::Tesseract)
            .map_err(|err| OcrError::LibraryLoad {
                path: self.assets.dir().to_path_buf(),
                message: err.to_string(),
            })?;
        Ok(vec![path])
    }

    fn tessdata_dir(&self) -> Result<PathBuf, OcrError> {
        let not_found = || OcrError::TessdataNotFound {
            searched: vec![self.assets.dir().to_path_buf()],
        };
        // Every preset's traineddata lives in the same directory; probe
        // with `eng` (bundled for all English-capable presets).
        let traineddata = self
            .assets
            .traineddata_path("eng")
            .map_err(|_| not_found())?;
        traineddata
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(not_found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::MockEngine;
    use crate::pdf::MockPageSpec;
    use crate::pdf::PdfDocument;
    use crate::transactional::is_temp_file_name;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;

    /// Hand-written minimal valid PDF (same shape as the validation
    /// module's fixture: structure only, the scanner does not resolve
    /// offsets).
    fn minimal_pdf() -> Vec<u8> {
        b"%PDF-1.4\n\
          1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
          2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
          xref\n0 1\n0000000000 65535 f\n\
          trailer\n<< /Size 1 /Root 1 0 R >>\n\
          startxref\n9\n\
          %%EOF\n"
            .to_vec()
    }

    /// Minimal PDF whose trailer carries an encryption dictionary entry.
    fn encrypted_pdf() -> Vec<u8> {
        b"%PDF-1.4\n\
          1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
          2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
          xref\n0 1\n0000000000 65535 f\n\
          trailer\n<< /Size 1 /Root 1 0 R /Encrypt 5 0 R >>\n\
          startxref\n9\n\
          %%EOF\n"
            .to_vec()
    }

    fn scratch_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chilli-pdf-worker-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn write_input(dir: &Path, bytes: &[u8]) -> PathBuf {
        let path = dir.join("input.pdf");
        fs::write(&path, bytes).expect("write input");
        path
    }

    fn job(input: &Path, output: &Path) -> Job {
        Job {
            input: input.to_path_buf(),
            output: output.to_path_buf(),
            language: LanguagePreset::English,
        }
    }

    /// Temp files left behind in `dir` — must always be empty after a run.
    fn no_temp_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .expect("read scratch dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.file_name().is_some_and(is_temp_file_name))
            .collect()
    }

    /// `PdfEngine` adapter that counts `open_document` calls, so tests can
    /// prove the pipeline never even opens rejected inputs.
    struct CountingEngine {
        inner: MockEngine,
        opens: Arc<AtomicUsize>,
    }

    impl PdfEngine for CountingEngine {
        fn open_document<'s>(
            &'s self,
            path: &Path,
        ) -> Result<Box<dyn PdfDocument<'s> + 's>, PdfError> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            self.inner.open_document(path)
        }
    }

    fn counting(inner: MockEngine) -> CountingEngine {
        CountingEngine {
            inner,
            opens: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Fake recogniser: records the images it saw (width, height, dpi),
    /// optionally after a delay, and can cancel the run mid-OCR.
    struct FakeOcr {
        seen: Arc<Mutex<Vec<(u32, u32, u32)>>>,
        delay: Duration,
        cancel_after: Option<CancelHandle>,
    }

    impl PageRecognizer for FakeOcr {
        fn recognize(&mut self, image: &OcrImage<'_>) -> Result<OcrPage, OcrError> {
            self.seen.lock().expect("fake OCR log poisoned").push((
                image.width(),
                image.height(),
                image.dpi(),
            ));
            thread::sleep(self.delay);
            if let Some(handle) = &self.cancel_after {
                handle.cancel();
            }
            Ok(OcrPage::empty(image.width(), image.height(), image.dpi()))
        }
    }

    fn fake_ocr(
        seen: Arc<Mutex<Vec<(u32, u32, u32)>>>,
        cancel_after: Option<CancelHandle>,
    ) -> OcrFactory {
        fake_ocr_with_delay(seen, cancel_after, Duration::ZERO)
    }

    fn fake_ocr_with_delay(
        seen: Arc<Mutex<Vec<(u32, u32, u32)>>>,
        cancel_after: Option<CancelHandle>,
        delay: Duration,
    ) -> OcrFactory {
        Arc::new(move |_preset| {
            Ok(Box::new(FakeOcr {
                seen: seen.clone(),
                delay,
                cancel_after: cancel_after.clone(),
            }) as Box<dyn PageRecognizer>)
        })
    }

    fn mock_pdf_factory(engine: MockEngine) -> PdfFactory {
        Arc::new(move || Ok(Box::new(engine.clone()) as Box<dyn PdfEngine>))
    }

    fn empty_log() -> Arc<Mutex<Vec<(u32, u32, u32)>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[test]
    fn happy_path_converts_every_page_and_publishes_once() {
        let dir = scratch_dir("happy");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        let engine = counting(MockEngine::with_pages(2));
        let seen = empty_log();
        let overlay = MockOverlay::default();
        let (events, inbox) = mpsc::channel();

        let outcome = convert_document(
            &job(&input, &output),
            &engine,
            &fake_ocr(seen.clone(), None),
            &overlay,
            &[],
            &CancelHandle::fresh(),
            &events,
        );

        assert!(matches!(outcome, Ok(Outcome::Completed)));
        let drained: Vec<WorkerEvent> = inbox.try_iter().collect();
        assert_eq!(
            drained,
            vec![
                WorkerEvent::Progress {
                    page: 1,
                    total_pages: 2
                },
                WorkerEvent::Progress {
                    page: 2,
                    total_pages: 2
                },
            ]
        );
        // Every page was rendered and recognised (letter pages have no
        // text, so both are classified as needing OCR).
        assert_eq!(seen.lock().expect("log poisoned").len(), 2);
        // The overlay ran once, saw both pages in order, and its output
        // is what landed in the file.
        let calls = overlay.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].input, input);
        assert_eq!(calls[0].pages, vec![0, 1]);
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-mock-searchable\n");
        // The original is untouched and the engine was opened once.
        assert_eq!(fs::read(&input).unwrap(), minimal_pdf());
        assert_eq!(engine.opens.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn mixed_document_ocrs_only_the_scanned_page() {
        let dir = scratch_dir("mixed");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        // Page 1 already carries text, page 2 is the scanned image page.
        let engine =
            MockEngine::with_spec(vec![MockPageSpec::searchable(), MockPageSpec::letter()]);
        let seen = empty_log();
        let overlay = MockOverlay::default();
        let (events, inbox) = mpsc::channel();

        let outcome = convert_document(
            &job(&input, &output),
            &engine,
            &fake_ocr(seen.clone(), None),
            &overlay,
            &[],
            &CancelHandle::fresh(),
            &events,
        );

        assert!(matches!(outcome, Ok(Outcome::Completed)));
        // Progress still covers every page, but only the scanned page
        // reached the recogniser and the overlay.
        let drained: Vec<WorkerEvent> = inbox.try_iter().collect();
        assert_eq!(
            drained,
            vec![
                WorkerEvent::Progress {
                    page: 1,
                    total_pages: 2
                },
                WorkerEvent::Progress {
                    page: 2,
                    total_pages: 2
                },
            ]
        );
        assert_eq!(seen.lock().expect("log poisoned").len(), 1);
        let calls = overlay.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].pages, vec![1]);
        assert!(output.exists());
    }

    #[test]
    fn cancel_mid_document_publishes_nothing_and_cleans_up() {
        let dir = scratch_dir("cancel-direct");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        let engine = MockEngine::with_pages(3);
        let cancel = CancelHandle::fresh();
        // The recogniser cancels the run during page 1's OCR, as a user
        // clicking Cancel mid-conversion would.
        let overlay = MockOverlay::default();
        let (events, inbox) = mpsc::channel();

        let outcome = convert_document(
            &job(&input, &output),
            &engine,
            &fake_ocr(empty_log(), Some(cancel.clone())),
            &overlay,
            &[],
            &cancel,
            &events,
        );

        assert!(matches!(outcome, Ok(Outcome::Cancelled)));
        // Page 1 reported progress; the run stopped before page 2.
        let drained: Vec<WorkerEvent> = inbox.try_iter().collect();
        assert_eq!(
            drained,
            vec![WorkerEvent::Progress {
                page: 1,
                total_pages: 3
            }]
        );
        assert!(overlay.calls().is_empty());
        assert!(!output.exists());
        assert!(no_temp_files(&dir).is_empty());
        assert_eq!(fs::read(&input).unwrap(), minimal_pdf());
    }

    #[test]
    fn validation_rejection_short_circuits_before_open() {
        let dir = scratch_dir("validation");
        let input = write_input(&dir, &encrypted_pdf());
        let output = dir.join("searchable.pdf");

        let engine = counting(MockEngine::with_pages(2));
        let (events, inbox) = mpsc::channel();

        let error = convert_document(
            &job(&input, &output),
            &engine,
            &fake_ocr(empty_log(), None),
            &MockOverlay::default(),
            &[],
            &CancelHandle::fresh(),
            &events,
        )
        .expect_err("encrypted input must be rejected");

        assert!(matches!(
            error,
            ConversionError::Validation(ValidationError::Encrypted)
        ));
        assert!(
            error.user_message().to_lowercase().contains("password"),
            "the message should mention the password, got: {}",
            error.user_message()
        );
        // Rejected before any conversion work: the engine was never even
        // asked to open the document.
        assert_eq!(engine.opens.load(Ordering::Relaxed), 0);
        assert_eq!(inbox.try_iter().count(), 0);
        assert!(!output.exists());
        assert!(no_temp_files(&dir).is_empty());
    }

    #[test]
    fn same_as_input_is_refused() {
        let dir = scratch_dir("same-as-input");
        let input = write_input(&dir, &minimal_pdf());

        let engine = counting(MockEngine::with_pages(2));
        let (events, inbox) = mpsc::channel();

        let error = convert_document(
            &job(&input, &input),
            &engine,
            &fake_ocr(empty_log(), None),
            &MockOverlay::default(),
            &[],
            &CancelHandle::fresh(),
            &events,
        )
        .expect_err("the original can never be the destination");

        assert!(matches!(error, ConversionError::SameAsInput));
        assert!(
            error
                .user_message()
                .contains("never overwrites the original")
        );
        assert_eq!(engine.opens.load(Ordering::Relaxed), 0);
        assert_eq!(fs::read(&input).unwrap(), minimal_pdf());
        assert_eq!(inbox.try_iter().count(), 0);
    }

    #[test]
    fn unimplemented_overlay_reports_pending_milestone() {
        let dir = scratch_dir("overlay-pending");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        let (events, inbox) = mpsc::channel();
        let error = convert_document(
            &job(&input, &output),
            &MockEngine::with_pages(1),
            &fake_ocr(empty_log(), None),
            &UnimplementedOverlay,
            &[],
            &CancelHandle::fresh(),
            &events,
        )
        .expect_err("the placeholder overlay cannot publish");

        assert!(matches!(error, ConversionError::OverlayPending));
        let message = error.user_message();
        assert!(
            message.contains("issue #13"),
            "the message should name the overlay milestone, got: {message}"
        );
        // The page itself still reported progress before the overlay
        // refused to publish.
        assert_eq!(
            inbox.try_iter().collect::<Vec<WorkerEvent>>(),
            vec![WorkerEvent::Progress {
                page: 1,
                total_pages: 1
            }]
        );
        assert!(!output.exists());
        assert!(no_temp_files(&dir).is_empty());
    }

    #[test]
    fn ocr_engine_failure_fails_the_job_cleanly() {
        let dir = scratch_dir("ocr-failure");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        let failing: OcrFactory = Arc::new(|_| {
            Err(ConversionError::Ocr(OcrError::RecognitionFailed {
                language: "eng".to_owned(),
            }))
        });
        let (events, inbox) = mpsc::channel();

        let error = convert_document(
            &job(&input, &output),
            &MockEngine::with_pages(1),
            &failing,
            &MockOverlay::default(),
            &[],
            &CancelHandle::fresh(),
            &events,
        )
        .expect_err("OCR must not succeed");

        assert!(matches!(error, ConversionError::Ocr(_)));
        assert!(!output.exists());
        assert!(no_temp_files(&dir).is_empty());
        assert_eq!(inbox.try_iter().count(), 0);
    }

    #[test]
    fn converter_reports_progress_then_done_over_the_channel() {
        let dir = scratch_dir("threaded-happy");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        let overlay = MockOverlay::default();
        let converter: Arc<dyn Converter> = Arc::new(PipelineConverter::new(
            mock_pdf_factory(MockEngine::with_pages(2)),
            fake_ocr(empty_log(), None),
            Arc::new(overlay.clone()),
            Vec::new(),
        ));

        let (events, inbox) = mpsc::channel();
        let mut worker = converter.clone().start(job(&input, &output), events);
        worker.join();

        let drained: Vec<WorkerEvent> = inbox.try_iter().collect();
        assert_eq!(
            drained,
            vec![
                WorkerEvent::Progress {
                    page: 1,
                    total_pages: 2
                },
                WorkerEvent::Progress {
                    page: 2,
                    total_pages: 2
                },
                WorkerEvent::Done,
            ]
        );
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-mock-searchable\n");
        assert_eq!(fs::read(&input).unwrap(), minimal_pdf());
        assert_eq!(overlay.calls().len(), 1);
    }

    #[test]
    fn cancel_mid_run_yields_cancelled_and_no_leftovers() {
        let dir = scratch_dir("threaded-cancel");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        // Plenty of slow pages, so the run is still mid-document when
        // the user cancels.
        let converter: Arc<dyn Converter> = Arc::new(PipelineConverter::new(
            mock_pdf_factory(MockEngine::with_pages(300)),
            fake_ocr_with_delay(empty_log(), None, Duration::from_millis(3)),
            Arc::new(MockOverlay::default()),
            Vec::new(),
        ));

        let (events, inbox) = mpsc::channel();
        let mut worker = converter.clone().start(job(&input, &output), events);

        // Wait for the first progress message, then cancel like a user.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match inbox.try_recv() {
                Ok(WorkerEvent::Progress { .. }) => break,
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                _ => panic!("timed out waiting for the first progress message"),
            }
        }
        worker.cancel();
        worker.join();

        let drained: Vec<WorkerEvent> = inbox.try_iter().collect();
        assert_eq!(drained.last(), Some(&WorkerEvent::Cancelled));
        assert!(
            !drained
                .iter()
                .any(|event| matches!(event, WorkerEvent::Failed(_))),
            "cancellation is not a failure: {drained:?}"
        );
        let last_page = drained.iter().rev().find_map(|event| match event {
            WorkerEvent::Progress { page, .. } => Some(*page),
            _ => None,
        });
        assert!(last_page.is_some_and(|page| page < 300));
        assert!(!output.exists());
        assert!(no_temp_files(&dir).is_empty());
        assert_eq!(fs::read(&input).unwrap(), minimal_pdf());
    }

    #[test]
    fn engine_is_created_once_and_shared_across_jobs() {
        let dir = scratch_dir("engine-once");
        let input = write_input(&dir, &minimal_pdf());

        let creations = Arc::new(AtomicUsize::new(0));
        let flag = creations.clone();
        let pdf: PdfFactory = Arc::new(move || {
            flag.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(MockEngine::with_pages(1)) as Box<dyn PdfEngine>)
        });

        let overlay = MockOverlay::default();
        let converter = Arc::new(PipelineConverter::new(
            pdf,
            fake_ocr(empty_log(), None),
            Arc::new(overlay.clone()),
            Vec::new(),
        ));

        for index in 0..2 {
            let output = dir.join(format!("searchable-{index}.pdf"));
            let (events, inbox) = mpsc::channel();
            let mut worker = converter.clone().start(job(&input, &output), events);
            worker.join();
            assert_eq!(inbox.try_iter().last(), Some(WorkerEvent::Done));
        }

        // One PDF engine for the process, reused across documents.
        assert_eq!(creations.load(Ordering::Relaxed), 1);
        assert_eq!(overlay.calls().len(), 2);
    }

    #[test]
    fn factory_failure_reports_failed_then_recovers() {
        let dir = scratch_dir("factory-recover");
        let input = write_input(&dir, &minimal_pdf());
        let output = dir.join("searchable.pdf");

        let fail_first = Arc::new(AtomicBool::new(true));
        let flag = fail_first.clone();
        let pdf: PdfFactory = Arc::new(move || {
            if flag.swap(false, Ordering::Relaxed) {
                Err(ConversionError::Open(PdfError::LibraryUnavailable {
                    path: PathBuf::from("missing native library"),
                }))
            } else {
                Ok(Box::new(MockEngine::with_pages(1)) as Box<dyn PdfEngine>)
            }
        });

        let converter: Arc<dyn Converter> = Arc::new(PipelineConverter::new(
            pdf,
            fake_ocr(empty_log(), None),
            Arc::new(MockOverlay::default()),
            Vec::new(),
        ));

        // First job: the PDF component fails to start and the user sees a
        // clean failure instead of a hang.
        let first_output = dir.join("first.pdf");
        let (events, inbox) = mpsc::channel();
        let mut worker = converter.clone().start(job(&input, &first_output), events);
        worker.join();
        let drained: Vec<WorkerEvent> = inbox.try_iter().collect();
        assert_eq!(
            drained.last(),
            Some(&WorkerEvent::Failed(
                concat!(
                    "The PDF component could not be started. Reinstall ",
                    "chilli-pdf and try again."
                )
                .to_owned()
            ))
        );
        assert!(!first_output.exists());

        // Second job: the factory is retried, succeeds, and completes.
        let (events, inbox) = mpsc::channel();
        let mut worker = converter.start(job(&input, &output), events);
        worker.join();
        assert_eq!(inbox.try_iter().last(), Some(WorkerEvent::Done));
        assert!(output.exists());
    }
}
