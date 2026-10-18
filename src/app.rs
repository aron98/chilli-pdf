//! Native window shell: eframe/egui over the headless [`AppState`].
//!
//! This module is intentionally a thin view. All decisions live in
//! [`crate::app_state`]; here we only draw controls, map button presses
//! to state actions, and drive native file dialogs. The window never
//! blocks: conversion runs on a worker thread and the frame loop only
//! polls its channel, repainting whenever a message arrives.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self as egui, Color32, RichText};

use crate::app_state::{AppState, Phase, Status};
use crate::converter::Converter;
use crate::language::LanguagePreset;
use crate::worker::PipelineConverter;

/// How often the window wakes up while a conversion is running, so the
/// progress bar animates even between worker messages.
const PROGRESS_REPAINT: Duration = Duration::from_millis(100);

/// The conversion backend shipped with the GUI.
///
/// The real conversion worker (issue #10): validation, PDFium rendering,
/// Tesseract OCR and the transactional publish all run on the pipeline's
/// dedicated thread. The invisible-text overlay itself is pending its
/// milestone (issue #13), so finished runs report the typed "overlay
/// pending" failure until that lands and the seam is wired. The state
/// machine's tests keep using the mock converter.
pub fn default_converter() -> Arc<dyn Converter> {
    Arc::new(PipelineConverter::production())
}

/// Open the native window. `initial_input` pre-selects an input PDF when
/// the binary was started with a path argument.
pub fn run(initial_input: Option<PathBuf>) -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("chilli-pdf")
            .with_inner_size([560.0, 430.0])
            .with_min_inner_size([460.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "chilli-pdf",
        options,
        Box::new(move |_creation_context| {
            let mut state = AppState::new(default_converter());
            if let Some(path) = initial_input {
                state.select_input(path);
            }
            Ok(Box::new(GuiApp { state }))
        }),
    )
}

/// Everything a native file dialog needs, as pure data.
///
/// Opening a dialog requires a real desktop session, so that part cannot
/// be covered headlessly — but *building the configuration* is pure and
/// unit-tested below ([`input_dialog_spec`], [`output_dialog_spec`]);
/// [`pick_file_dialog`] / [`save_file_dialog`] turn a spec into an
/// `rfd::FileDialog` at the last possible moment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileDialogSpec {
    /// Window title.
    title: String,
    /// Human-readable name of the file-type filter.
    filter_name: String,
    /// Filter extensions, without the leading dot.
    filter_extensions: Vec<String>,
    /// Pre-filled file name for save dialogs when it is valid Unicode.
    ///
    /// `None` for a non-Unicode name: `rfd` takes file names as `String`,
    /// so a lossy conversion would mangle the suggestion — omitting it
    /// leaves the original name untouched instead.
    default_file_name: Option<String>,
    /// Directory the dialog opens in, when one is known.
    default_directory: Option<PathBuf>,
}

/// Configuration of the "choose the scanned PDF" input dialog.
///
/// No default directory or file name: the OS remembers the last-used
/// location per application, which is what users expect for inputs.
fn input_dialog_spec() -> FileDialogSpec {
    FileDialogSpec {
        title: "Choose the scanned PDF".to_owned(),
        filter_name: "PDF document".to_owned(),
        filter_extensions: vec!["pdf".to_owned()],
        default_file_name: None,
        default_directory: None,
    }
}

/// Configuration of the "save the searchable PDF" dialog, pre-filled
/// from the currently suggested `output` path (from
/// `output_path::default_output_path`).
fn output_dialog_spec(output: &Path) -> FileDialogSpec {
    FileDialogSpec {
        title: "Where should the searchable PDF be saved?".to_owned(),
        filter_name: "PDF document".to_owned(),
        filter_extensions: vec!["pdf".to_owned()],
        default_file_name: output
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned),
        default_directory: output
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_path_buf),
    }
}

/// Applies a [`FileDialogSpec`] to a fresh `rfd::FileDialog`.
fn configure_dialog(dialog: rfd::FileDialog, spec: &FileDialogSpec) -> rfd::FileDialog {
    let mut dialog = dialog
        .add_filter(spec.filter_name.as_str(), &spec.filter_extensions)
        .set_title(spec.title.as_str());
    if let Some(directory) = &spec.default_directory {
        dialog = dialog.set_directory(directory);
    }
    if let Some(name) = &spec.default_file_name {
        dialog = dialog.set_file_name(name.clone());
    }
    dialog
}

/// Opens the modal "pick a file" dialog described by `spec`.
fn pick_file_dialog(spec: &FileDialogSpec) -> Option<PathBuf> {
    configure_dialog(rfd::FileDialog::new(), spec).pick_file()
}

/// Opens the modal "choose where to save" dialog described by `spec`.
fn save_file_dialog(spec: &FileDialogSpec) -> Option<PathBuf> {
    configure_dialog(rfd::FileDialog::new(), spec).save_file()
}

/// The egui application: a view over [`AppState`].
struct GuiApp {
    state: AppState,
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let received_message = self.state.poll();
        if received_message {
            ctx.request_repaint();
        }
        if self.state.is_converting() {
            ctx.request_repaint_after(PROGRESS_REPAINT);
        }

        let prompting = matches!(self.state.phase(), Phase::ConfirmOverwrite(_));
        egui::CentralPanel::default_margins().show(ui, |ui| {
            if prompting {
                ui.disable(); // The replace prompt is modal.
            }
            self.draw_controls(ui);
        });
        self.draw_replace_prompt(&ctx);
    }
}

impl GuiApp {
    fn draw_controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Make a scanned PDF searchable");
        ui.add_space(8.0);
        self.draw_settings(ui);
        ui.add_space(12.0);
        self.draw_buttons(ui);
        ui.add_space(8.0);
        self.draw_progress(ui);
        self.draw_status(ui);
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("settings")
            .num_columns(3)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                // --- input -------------------------------------------------
                ui.strong("Input PDF");
                let input_text = self
                    .state
                    .input()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "No file selected".to_owned());
                ui.label(RichText::new(input_text).monospace().weak());
                if ui.button("Choose…").clicked() {
                    self.pick_input();
                }
                ui.end_row();

                // --- language ---------------------------------------------
                ui.strong("OCR language");
                let mut language = self.state.language();
                egui::ComboBox::from_id_salt("ocr-language")
                    .selected_text(language.label())
                    .show_ui(ui, |ui| {
                        for preset in LanguagePreset::ALL {
                            ui.selectable_value(&mut language, preset, preset.label());
                        }
                    });
                self.state.set_language(language);
                ui.add_space(0.0); // Keep the grid columns aligned.
                ui.end_row();

                // --- output -----------------------------------------------
                ui.strong("Output");
                let output_text = self
                    .state
                    .output()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Chosen when a PDF is selected".to_owned());
                ui.label(RichText::new(output_text).monospace().weak())
                    .on_hover_text(
                        "Default: next to the original, named <original>_searchable.pdf",
                    );
                if ui.button("Change…").clicked() {
                    self.pick_output();
                }
                ui.end_row();
            });
    }

    fn draw_buttons(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let convert = ui.add_enabled(self.state.can_convert(), egui::Button::new("Convert"));
            if convert.clicked() {
                self.state.request_convert();
            }
            let cancel = ui.add_enabled(self.state.can_cancel(), egui::Button::new("Cancel"));
            if cancel.clicked() {
                self.state.cancel();
            }
        });
    }

    fn draw_progress(&mut self, ui: &mut egui::Ui) {
        if let Phase::Converting { page, total_pages } = self.state.phase() {
            let fraction = if *total_pages > 0 {
                *page as f32 / *total_pages as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .text(format!("Converting: page {page} of {total_pages}")),
            );
        }
    }

    fn draw_status(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        match self.state.status() {
            Status::Quiet => {}
            Status::Info(text) => {
                ui.label(RichText::new(text).weak());
            }
            Status::Error(text) => {
                ui.label(RichText::new(text).color(Color32::from_rgb(176, 32, 32)));
            }
        }
    }

    /// Modal "ask before replacing" prompt for [`Phase::ConfirmOverwrite`].
    fn draw_replace_prompt(&mut self, ctx: &egui::Context) {
        let existing = match self.state.phase() {
            Phase::ConfirmOverwrite(path) => path.clone(),
            _ => return,
        };
        let mut open = true;
        egui::Window::new("Replace existing file?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("“{}” already exists.", existing.display()));
                ui.label("Replace it with the searchable copy?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Replace").clicked() {
                        self.state.confirm_overwrite();
                    }
                    if ui.button("Cancel").clicked() {
                        self.state.decline_overwrite();
                    }
                });
            });
        if !open {
            self.state.decline_overwrite();
        }
    }

    fn pick_input(&mut self) {
        if let Some(path) = pick_file_dialog(&input_dialog_spec()) {
            self.state.select_input(path);
        }
    }

    fn pick_output(&mut self) {
        let output = match self.state.output() {
            Some(output) => output.to_path_buf(),
            // Nothing selected yet; the output is chosen with the input.
            None => return,
        };
        if let Some(path) = save_file_dialog(&output_dialog_spec(&output)) {
            self.state.select_output(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_dialog_offers_only_pdf_files_without_any_prefill() {
        let spec = input_dialog_spec();
        assert_eq!(spec.title, "Choose the scanned PDF");
        assert_eq!(spec.filter_name, "PDF document");
        assert_eq!(spec.filter_extensions, ["pdf"]);
        assert_eq!(spec.default_file_name, None);
        assert_eq!(spec.default_directory, None);
    }

    #[test]
    fn output_dialog_suggests_the_default_name_and_its_directory() {
        let output = PathBuf::from("/scan/dir/Report_searchable.pdf");
        let spec = output_dialog_spec(&output);
        assert_eq!(spec.title, "Where should the searchable PDF be saved?");
        assert_eq!(spec.filter_extensions, ["pdf"]);
        assert_eq!(
            spec.default_file_name.as_deref(),
            Some("Report_searchable.pdf")
        );
        assert_eq!(
            spec.default_directory.as_deref(),
            Some(Path::new("/scan/dir"))
        );
    }

    #[test]
    fn output_dialog_without_a_parent_directory_opens_without_one() {
        // A bare file name has `Some("")` as its parent; an empty path
        // would point the dialog at the current directory, so it must
        // be dropped instead.
        let output = PathBuf::from("Report_searchable.pdf");
        let spec = output_dialog_spec(&output);
        assert_eq!(
            spec.default_file_name.as_deref(),
            Some("Report_searchable.pdf")
        );
        assert_eq!(spec.default_directory, None);
    }

    #[test]
    fn output_dialog_passes_unicode_names_through_verbatim() {
        let output = PathBuf::from("/scans/Chilli 🌶️_searchable.pdf");
        let spec = output_dialog_spec(&output);
        assert_eq!(
            spec.default_file_name.as_deref(),
            Some("Chilli 🌶️_searchable.pdf")
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_dialog_omits_non_unicode_names_instead_of_mangling_them() {
        // `rfd` takes the suggested name as a `String`; a lossy
        // conversion would corrupt it. A name that is not valid Unicode
        // must be omitted entirely, leaving the real file untouched.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let output = PathBuf::from(OsStr::from_bytes(b"/scans/Bad_\xFF_name_searchable.pdf"));
        let spec = output_dialog_spec(&output);
        assert_eq!(spec.default_file_name, None);
        assert_eq!(spec.default_directory.as_deref(), Some(Path::new("/scans")));
    }
}
