//! Headless state machine for the converter window.
//!
//! Everything the window shows and every button state derives from
//! [`AppState`], which is plain Rust with no GUI dependency — the whole
//! user workflow (input selection, output conflicts, conversion progress,
//! cancellation) is unit-testable without opening a window. The egui shell
//! in [`crate::app`] is a thin view over this state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use crate::converter::{Converter, Job, Worker, WorkerEvent};
use crate::language::LanguagePreset;
use crate::output_path::{Conflict, check_output, default_output_path};

/// What the window is currently doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Nothing running; the user can pick files and start a conversion.
    Idle,
    /// A conversion is in flight; `page`/`total_pages` drive the progress
    /// bar. Both start at 0 until the first page report arrives.
    Converting { page: usize, total_pages: usize },
    /// Waiting for the user to confirm replacing an existing output file.
    ConfirmOverwrite(PathBuf),
    /// The conversion finished and the output was written.
    Done,
}

/// Status area content below the controls.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Quiet,
    /// Neutral/positive information, rendered as secondary text.
    Info(String),
    /// Actionable problem description; the user can fix the cause.
    Error(String),
}

/// Never-dump-the-contents guarantee: this message never embeds the
/// document or file bytes, only the rule that was violated.
pub const SAME_AS_INPUT_MSG: &str = "The output must be a different file than the original document. \
     Pick another output location.";

/// The complete UI state. GUI-framework agnostic.
pub struct AppState {
    converter: Arc<dyn Converter>,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    language: LanguagePreset,
    phase: Phase,
    status: Status,
    worker: Option<Worker>,
    receiver: Option<Receiver<WorkerEvent>>,
}

impl AppState {
    /// New idle state over the given conversion backend.
    pub fn new(converter: Arc<dyn Converter>) -> Self {
        Self {
            converter,
            input: None,
            output: None,
            language: LanguagePreset::English,
            phase: Phase::Idle,
            status: Status::Quiet,
            worker: None,
            receiver: None,
        }
    }

    /// Builder-style: start with an input pre-selected (CLI argument).
    pub fn with_input(mut self, input: PathBuf) -> Self {
        self.select_input(input);
        self
    }

    // ------------------------------------------------------------ accessors

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn language(&self) -> LanguagePreset {
        self.language
    }

    pub fn input(&self) -> Option<&Path> {
        self.input.as_deref()
    }

    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    pub fn is_converting(&self) -> bool {
        matches!(self.phase, Phase::Converting { .. })
    }

    // -------------------------------------------------------------- actions

    /// The user picked an input PDF. Resets output to the default and
    /// clears any finished state. Ignored while a conversion runs (the
    /// user must cancel first).
    pub fn select_input(&mut self, path: PathBuf) {
        if self.is_converting() {
            return;
        }
        self.output = Some(default_output_path(&path));
        self.input = Some(path);
        self.phase = Phase::Idle;
        self.status = Status::Quiet;
    }

    /// The user picked a custom output location. Validated immediately so
    /// the status area explains a problem before Convert is pressed.
    pub fn select_output(&mut self, path: PathBuf) {
        if self.is_converting() {
            return;
        }
        self.output = Some(path);
        self.status = self.describe_output_conflict();
    }

    /// Change the OCR language preset. Ignored while converting.
    pub fn set_language(&mut self, language: LanguagePreset) {
        if self.is_converting() {
            return;
        }
        self.language = language;
    }

    /// Convert is enabled once an input is chosen, the output differs from
    /// the input, and nothing is already running or awaiting confirmation.
    /// An existing output file does not disable Convert — it triggers the
    /// replace prompt when Convert is pressed.
    pub fn can_convert(&self) -> bool {
        if self.is_converting() || matches!(self.phase, Phase::ConfirmOverwrite(_)) {
            return false;
        }
        let (Some(input), Some(output)) = (&self.input, &self.output) else {
            return false;
        };
        !matches!(check_output(input, output), Err(Conflict::SameAsInput))
    }

    /// Cancel is enabled exactly while a conversion is running.
    pub fn can_cancel(&self) -> bool {
        self.is_converting()
    }

    /// The user pressed Convert: run the pre-flight output check, ask
    /// before replacing an existing file, refuse same-as-input, otherwise
    /// start the job.
    pub fn request_convert(&mut self) {
        if !self.can_convert() {
            return;
        }
        let input = self.input.clone().unwrap_or_default();
        let output = self.output.clone().unwrap_or_default();
        match check_output(&input, &output) {
            Err(Conflict::SameAsInput) => {
                self.phase = Phase::Idle;
                self.status = Status::Error(SAME_AS_INPUT_MSG.to_owned());
            }
            Err(Conflict::AlreadyExists(existing)) => {
                self.phase = Phase::ConfirmOverwrite(existing);
                self.status = Status::Quiet;
            }
            Ok(()) => self.start_job(),
        }
    }

    /// The user confirmed replacing the existing output.
    pub fn confirm_overwrite(&mut self) {
        if matches!(self.phase, Phase::ConfirmOverwrite(_)) {
            self.start_job();
        }
    }

    /// The user declined to replace the existing output.
    pub fn decline_overwrite(&mut self) {
        if matches!(self.phase, Phase::ConfirmOverwrite(_)) {
            self.phase = Phase::Idle;
            self.status =
                Status::Info("Conversion skipped. Choose a different output location.".to_owned());
        }
    }

    /// The user pressed Cancel; the worker acknowledges via
    /// [`WorkerEvent::Cancelled`].
    pub fn cancel(&mut self) {
        if let Some(worker) = &self.worker {
            worker.cancel();
        }
    }

    /// Drain pending worker messages. Returns `true` when at least one
    /// message arrived (the view should repaint).
    pub fn poll(&mut self) -> bool {
        let mut received = false;
        while let Some(event) = self.recv_event() {
            received = true;
            self.handle_event(event);
            if !self.is_converting() {
                break; // Terminal event handled; nothing more follows.
            }
        }
        received
    }

    /// Try to take the next worker message, if a worker is running and
    /// has sent one. A dropped channel ends the polling early.
    fn recv_event(&mut self) -> Option<WorkerEvent> {
        let outcome = self.receiver.as_ref().map(|receiver| receiver.try_recv());
        match outcome {
            Some(Ok(event)) => Some(event),
            Some(Err(TryRecvError::Empty)) => None,
            Some(Err(TryRecvError::Disconnected)) => {
                self.receiver = None;
                None
            }
            None => None,
        }
    }

    // ----------------------------------------------------------- internals

    fn start_job(&mut self) {
        let (sender, receiver) = mpsc::channel();
        let job = Job {
            input: self.input.clone().unwrap_or_default(),
            output: self.output.clone().unwrap_or_default(),
            language: self.language,
        };
        self.worker = Some(self.converter.clone().start(job, sender));
        self.receiver = Some(receiver);
        self.phase = Phase::Converting {
            page: 0,
            total_pages: 0,
        };
        self.status = Status::Info("Converting…".to_owned());
    }

    fn handle_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Progress { page, total_pages } => {
                if let Phase::Converting {
                    page: current_page,
                    total_pages: current_total,
                } = &mut self.phase
                {
                    *current_page = page;
                    *current_total = total_pages;
                }
            }
            WorkerEvent::Done => {
                let output = self.output.clone().unwrap_or_default();
                self.finish(
                    Phase::Done,
                    Status::Info(format!(
                        "Done. The searchable PDF was saved to {}.",
                        output.display()
                    )),
                );
            }
            WorkerEvent::Failed(message) => {
                self.finish(Phase::Idle, Status::Error(message));
            }
            WorkerEvent::Cancelled => {
                self.finish(
                    Phase::Idle,
                    Status::Info(
                        "Conversion cancelled. The original file is untouched.".to_owned(),
                    ),
                );
            }
        }
    }

    /// Leave the converting phase: reap the worker and settle the state.
    fn finish(&mut self, phase: Phase, status: Status) {
        if let Some(worker) = &mut self.worker {
            worker.join();
        }
        self.worker = None;
        self.phase = phase;
        self.status = status;
    }

    /// Explain the current output choice (if any) in the status area.
    fn describe_output_conflict(&self) -> Status {
        let (Some(input), Some(output)) = (&self.input, &self.output) else {
            return Status::Quiet;
        };
        match check_output(input, output) {
            Err(Conflict::SameAsInput) => Status::Error(SAME_AS_INPUT_MSG.to_owned()),
            Err(Conflict::AlreadyExists(_)) => Status::Info(
                "That file already exists. You will be asked before it is replaced.".to_owned(),
            ),
            Ok(()) => Status::Quiet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::MockConverter;
    use std::fs;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// Unique scratch directory per test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chilli-pdf-appstate-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before the epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Poll `state` (as the UI frame loop would) until it leaves the
    /// converting phase, or fail after `timeout`.
    fn run_to_completion(state: &mut AppState, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while state.is_converting() {
            assert!(
                Instant::now() < deadline,
                "conversion did not finish within the timeout"
            );
            state.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        state.poll(); // Drain anything left behind.
    }

    /// Poll until at least `min_page` pages have been reported.
    fn run_until_page(state: &mut AppState, min_page: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            state.poll();
            if let Phase::Converting { page, .. } = state.phase()
                && *page >= min_page
            {
                return;
            }
            assert!(Instant::now() < deadline, "no progress within the timeout");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn state_with(converter: MockConverter) -> AppState {
        AppState::new(Arc::new(converter))
    }

    #[test]
    fn initial_state_has_nothing_selected_and_convert_disabled() {
        let state = state_with(MockConverter::fast());
        assert!(state.input().is_none());
        assert!(state.output().is_none());
        assert_eq!(state.phase(), &Phase::Idle);
        assert!(!state.can_convert());
        assert!(!state.can_cancel());
        assert_eq!(state.status(), &Status::Quiet);
    }

    #[test]
    fn request_convert_without_input_is_a_no_op() {
        let mut state = state_with(MockConverter::fast());
        state.request_convert();
        assert_eq!(state.phase(), &Phase::Idle);
        assert!(!state.is_converting());
    }

    #[test]
    fn selecting_input_sets_the_default_output_path() {
        let mut state = state_with(MockConverter::fast());
        let input = scratch("default-output").join("scan.pdf");
        fs::write(&input, b"%PDF-fake").unwrap();
        state.select_input(input.clone());

        assert_eq!(state.input(), Some(input.as_path()));
        assert_eq!(
            state.output(),
            Some(default_output_path(&input).as_path()),
            "output must default to <stem>_searchable.pdf next to the input"
        );
        assert!(state.can_convert());
    }

    #[test]
    fn changing_the_output_location_is_kept_and_validated() {
        let mut state = state_with(MockConverter::fast());
        let dir = scratch("custom-output");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);

        let custom = dir.join("elsewhere").join("out.pdf");
        state.select_output(custom.clone());
        assert_eq!(state.output(), Some(custom.as_path()));
        assert_eq!(state.status(), &Status::Quiet);
        assert!(state.can_convert());
    }

    #[test]
    fn output_same_as_input_is_rejected() {
        let mut state = state_with(MockConverter::fast());
        let dir = scratch("same-as-input");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input.clone());

        // Picking the input as output must be refused outright...
        state.select_output(input.clone());
        assert!(!state.can_convert());
        assert!(matches!(state.status(), Status::Error(_)));

        // ...and pressing Convert (if it were enabled) must not start a job.
        state.request_convert();
        assert_eq!(state.phase(), &Phase::Idle);
        assert!(!state.is_converting());
    }

    #[test]
    fn existing_output_triggers_replace_prompt_then_conversion_on_confirm() {
        let mut state = state_with(MockConverter::fast());
        let dir = scratch("overwrite-confirm");
        let input = dir.join("scan.pdf");
        let output = dir.join("scan_searchable.pdf");
        fs::write(&input, b"x").unwrap();
        fs::write(&output, b"existing").unwrap();
        state.select_input(input);

        state.request_convert();
        assert_eq!(state.phase(), &Phase::ConfirmOverwrite(output.clone()));

        state.confirm_overwrite();
        assert!(state.is_converting());
        run_to_completion(&mut state, Duration::from_secs(10));
        assert_eq!(state.phase(), &Phase::Done);
        assert!(matches!(state.status(), Status::Info(_)));
        // The existing file is untouched until the real worker writes output.
        assert_eq!(fs::read(&output).unwrap(), b"existing");
    }

    #[test]
    fn declining_the_replace_prompt_returns_to_idle() {
        let mut state = state_with(MockConverter::fast());
        let dir = scratch("overwrite-decline");
        let input = dir.join("scan.pdf");
        let output = dir.join("scan_searchable.pdf");
        fs::write(&input, b"x").unwrap();
        fs::write(&output, b"existing").unwrap();
        state.select_input(input);

        state.request_convert();
        assert_eq!(state.phase(), &Phase::ConfirmOverwrite(output));
        state.decline_overwrite();
        assert_eq!(state.phase(), &Phase::Idle);
        assert!(matches!(state.status(), Status::Info(_)));
        assert!(!state.is_converting());
    }

    #[test]
    fn fresh_output_converts_straight_away_and_reports_done() {
        let mut state = state_with(MockConverter::fast());
        let dir = scratch("convert-done");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);
        assert!(state.can_convert());

        state.request_convert();
        assert!(state.is_converting());
        assert!(state.can_cancel());
        assert!(!state.can_convert(), "Convert must disable while running");

        run_to_completion(&mut state, Duration::from_secs(10));
        assert_eq!(state.phase(), &Phase::Done);
        let output = state.output().unwrap().to_path_buf();
        match state.status() {
            Status::Info(text) => assert!(
                text.contains(&output.display().to_string()),
                "success message must name the output file, got: {text}"
            ),
            other => panic!("expected info status, got {other:?}"),
        }
        assert!(!state.can_cancel());
    }

    #[test]
    fn progress_updates_page_by_page_during_the_run() {
        let mut state = state_with(MockConverter {
            total_pages: 8,
            page_delay: Duration::from_millis(3),
            fail_on_page: None,
        });
        let dir = scratch("progress");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);
        state.request_convert();

        let mut max_page = 0;
        for _ in 0..2000 {
            if !state.is_converting() {
                break;
            }
            state.poll();
            if let Phase::Converting { page, .. } = state.phase() {
                max_page = max_page.max(*page);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            state.phase(),
            &Phase::Done,
            "mock run must finish within the poll window"
        );
        assert!(
            max_page >= 7,
            "progress must reach the last pages, saw {max_page}"
        );
    }

    #[test]
    fn cancelling_mid_run_stops_the_job_and_reports_it() {
        let mut state = state_with(MockConverter {
            total_pages: 500,
            page_delay: Duration::from_millis(8),
            fail_on_page: None,
        });
        let dir = scratch("cancel");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"original").unwrap();
        state.select_input(input.clone());
        state.request_convert();

        run_until_page(&mut state, 2, Duration::from_secs(10));
        assert!(state.can_cancel());
        state.cancel();

        run_to_completion(&mut state, Duration::from_secs(10));
        assert_eq!(state.phase(), &Phase::Idle);
        assert!(matches!(state.status(), Status::Info(_)));
        assert!(!state.can_cancel());
        // The input is untouched.
        assert_eq!(fs::read(&input).unwrap(), b"original");
    }

    #[test]
    fn worker_failure_shows_an_actionable_error_and_returns_to_idle() {
        let mut state = state_with(MockConverter::fast_failing_on(3));
        let dir = scratch("failure");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);
        state.request_convert();

        run_to_completion(&mut state, Duration::from_secs(10));
        assert_eq!(state.phase(), &Phase::Idle);
        match state.status() {
            Status::Error(text) => assert!(
                text.contains("could not be processed"),
                "error must be a clear message, got: {text}"
            ),
            other => panic!("expected error status, got {other:?}"),
        }
        assert!(state.can_convert(), "the user can fix the cause and retry");
    }

    #[test]
    fn input_cannot_be_changed_while_converting() {
        let mut state = state_with(MockConverter {
            total_pages: 200,
            page_delay: Duration::from_millis(10),
            fail_on_page: None,
        });
        let dir = scratch("locked-input");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);

        state.request_convert();
        let second = dir.join("other.pdf");
        fs::write(&second, b"x").unwrap();
        state.select_input(second);
        assert_ne!(
            state.input(),
            Some(dir.join("other.pdf").as_path()),
            "input must stay locked during a conversion"
        );
        state.cancel();
        run_to_completion(&mut state, Duration::from_secs(10));
    }

    #[test]
    fn language_can_be_changed_when_idle_but_not_while_converting() {
        let mut state = state_with(MockConverter {
            total_pages: 200,
            page_delay: Duration::from_millis(10),
            fail_on_page: None,
        });
        state.set_language(LanguagePreset::Hungarian);
        assert_eq!(state.language(), LanguagePreset::Hungarian);

        let dir = scratch("language");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);
        state.request_convert();
        state.set_language(LanguagePreset::EnglishHungarian);
        assert_eq!(
            state.language(),
            LanguagePreset::Hungarian,
            "language must stay fixed during a conversion"
        );
        state.cancel();
        run_to_completion(&mut state, Duration::from_secs(10));
    }

    #[test]
    fn finished_conversion_can_be_run_again() {
        let mut state = state_with(MockConverter::fast());
        let dir = scratch("convert-twice");
        let input = dir.join("scan.pdf");
        fs::write(&input, b"x").unwrap();
        state.select_input(input);

        state.request_convert();
        run_to_completion(&mut state, Duration::from_secs(10));
        assert_eq!(state.phase(), &Phase::Done);
        assert!(state.can_convert());
        state.request_convert();
        assert!(state.is_converting());
        state.cancel();
        run_to_completion(&mut state, Duration::from_secs(10));
    }
}
