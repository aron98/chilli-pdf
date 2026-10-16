//! Converter abstraction: the window talks to a conversion backend
//! exclusively through this trait and its message protocol, so the real
//! OCR pipeline (issue #10) can plug in without touching the GUI shell.
//!
//! The backend runs on a worker thread and reports back through a channel;
//! the UI thread only polls, so the window never blocks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::language::LanguagePreset;

/// One conversion job handed to a [`Converter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The scanned PDF to read. Never modified.
    pub input: PathBuf,
    /// Where the searchable copy is written.
    pub output: PathBuf,
    /// Recognition language preset.
    pub language: LanguagePreset,
}

/// Worker → UI messages.
///
/// Sent from the background thread; the UI drains its channel every frame
/// and repaints whenever a message arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEvent {
    /// One more page finished. `page` is 1-based.
    Progress { page: usize, total_pages: usize },
    /// Conversion finished and the output was published.
    Done,
    /// Conversion failed. The message is user-readable and actionable and
    /// never contains document contents.
    Failed(String),
    /// The user cancelled; no output was written and the input is intact.
    Cancelled,
}

/// Cooperative cancellation shared between the UI and a worker.
#[derive(Debug, Clone)]
pub struct CancelHandle(Arc<AtomicBool>);

impl CancelHandle {
    fn fresh() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    /// Ask the worker to stop at its next checkpoint.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A conversion job in flight.
#[derive(Debug)]
pub struct Worker {
    cancel: CancelHandle,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// Ask the job to stop; the worker acknowledges with
    /// [`WorkerEvent::Cancelled`].
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Wait for the worker thread to finish (after a terminal event).
    pub fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A conversion backend. The GUI shell runs the [`MockConverter`] today;
/// the real OCR pipeline implements the same contract.
pub trait Converter: Send + Sync + 'static {
    /// Start `job` on a worker thread, reporting back through `events`.
    /// The returned [`Worker`] lets the UI cancel the job.
    fn start(self: Arc<Self>, job: Job, events: Sender<WorkerEvent>) -> Worker;
}

/// Simulated conversion backend.
///
/// Behaves like the future OCR pipeline — page-by-page progress over
/// several seconds, cooperative cancellation, terminal events — without
/// touching any PDF. [`MockConverter::default`] paces a realistic run for
/// the GUI; [`MockConverter::fast`] keeps unit tests quick.
#[derive(Debug, Clone)]
pub struct MockConverter {
    /// Pages the simulated document has.
    pub total_pages: usize,
    /// Simulated work per page.
    pub page_delay: Duration,
    /// When set, the mock fails on this page (1-based) instead of
    /// finishing, to exercise the error path.
    pub fail_on_page: Option<usize>,
}

impl Default for MockConverter {
    fn default() -> Self {
        Self {
            total_pages: 24,
            page_delay: Duration::from_millis(250),
            fail_on_page: None,
        }
    }
}

impl MockConverter {
    /// Quick variant for unit tests.
    pub fn fast() -> Self {
        Self {
            total_pages: 6,
            page_delay: Duration::from_millis(2),
            fail_on_page: None,
        }
    }

    /// Quick variant that fails on `page`, for error-path tests.
    pub fn fast_failing_on(page: usize) -> Self {
        Self {
            total_pages: page + 3,
            page_delay: Duration::from_millis(2),
            fail_on_page: Some(page),
        }
    }
}

impl Converter for MockConverter {
    fn start(self: Arc<Self>, _job: Job, events: Sender<WorkerEvent>) -> Worker {
        let cancel = CancelHandle::fresh();
        let handle = std::thread::Builder::new()
            .name("chilli-pdf-converter".to_owned())
            .spawn({
                let cancel = cancel.clone();
                move || {
                    for page in 1..=self.total_pages {
                        if cancel.is_cancelled() {
                            let _ = events.send(WorkerEvent::Cancelled);
                            return;
                        }
                        std::thread::sleep(self.page_delay);
                        if self.fail_on_page == Some(page) {
                            let _ = events.send(WorkerEvent::Failed(format!(
                                "The document could not be processed (page {page})."
                            )));
                            return;
                        }
                        if events
                            .send(WorkerEvent::Progress {
                                page,
                                total_pages: self.total_pages,
                            })
                            .is_err()
                        {
                            return; // The window is gone; stop quietly.
                        }
                    }
                    let _ = events.send(WorkerEvent::Done);
                }
            })
            .expect("spawn converter worker thread");
        Worker {
            cancel,
            handle: Some(handle),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    fn drain(receiver: &mpsc::Receiver<WorkerEvent>) -> Vec<WorkerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    fn run_to_end(converter: Arc<MockConverter>) -> Vec<WorkerEvent> {
        let (tx, rx) = mpsc::channel();
        let mut worker = converter.start(
            Job {
                input: PathBuf::from("in.pdf"),
                output: PathBuf::from("out.pdf"),
                language: LanguagePreset::English,
            },
            tx,
        );
        worker.join();
        drain(&rx)
    }

    #[test]
    fn mock_reports_progress_for_every_page_then_done() {
        let events = run_to_end(Arc::new(MockConverter::fast()));
        assert_eq!(events.len(), 7);
        assert_eq!(
            events[0],
            WorkerEvent::Progress {
                page: 1,
                total_pages: 6
            }
        );
        assert_eq!(
            events[5],
            WorkerEvent::Progress {
                page: 6,
                total_pages: 6
            }
        );
        assert_eq!(events[6], WorkerEvent::Done);
    }

    #[test]
    fn cancel_mid_run_yields_a_cancelled_event_and_stops_early() {
        let converter = Arc::new(MockConverter {
            total_pages: 500,
            page_delay: Duration::from_millis(5),
            fail_on_page: None,
        });
        let (tx, rx) = mpsc::channel();
        let worker = converter.start(
            Job {
                input: PathBuf::from("in.pdf"),
                output: PathBuf::from("out.pdf"),
                language: LanguagePreset::EnglishHungarian,
            },
            tx,
        );

        // Wait until some progress has happened, then cancel.
        let mut seen_page = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen_page == 0 {
            assert!(Instant::now() < deadline, "mock never reported progress");
            for event in drain(&rx) {
                if let WorkerEvent::Progress { page, .. } = event {
                    seen_page = page;
                }
            }
        }
        worker.cancel();

        // The worker may still be sleeping inside a page; wait (with a
        // deadline) until it reports a terminal event after noticing the flag.
        let mut cancelled = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cancelled.extend(drain(&rx));
            let terminal = cancelled.iter().any(|e| {
                matches!(
                    e,
                    WorkerEvent::Cancelled | WorkerEvent::Done | WorkerEvent::Failed(_)
                )
            });
            if terminal {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker never reported a terminal event after cancellation"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(cancelled.contains(&WorkerEvent::Cancelled));
        // The mock must stop far before its 500 pages.
        let last_page = cancelled
            .iter()
            .filter_map(|e| match e {
                WorkerEvent::Progress { page, .. } => Some(*page),
                _ => None,
            })
            .max()
            .unwrap_or(seen_page);
        assert!(last_page < 50, "cancellation did not stop the worker early");
    }

    #[test]
    fn failing_mock_sends_failed_instead_of_done() {
        let events = run_to_end(Arc::new(MockConverter::fast_failing_on(3)));
        assert_eq!(
            events.last(),
            Some(&WorkerEvent::Failed(
                "The document could not be processed (page 3).".to_owned()
            ))
        );
        assert!(!events.contains(&WorkerEvent::Done));
    }
}
