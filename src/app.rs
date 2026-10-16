//! Native window shell: eframe/egui over the headless [`AppState`].
//!
//! This module is intentionally a thin view. All decisions live in
//! [`crate::app_state`]; here we only draw controls, map button presses
//! to state actions, and drive native file dialogs. The window never
//! blocks: conversion runs on a worker thread and the frame loop only
//! polls its channel, repainting whenever a message arrives.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self as egui, Color32, RichText};

use crate::app_state::{AppState, Phase, Status};
use crate::converter::{Converter, MockConverter};
use crate::language::LanguagePreset;

/// How often the window wakes up while a conversion is running, so the
/// progress bar animates even between worker messages.
const PROGRESS_REPAINT: Duration = Duration::from_millis(100);

/// The conversion backend shipped with the GUI.
///
/// The real OCR pipeline lands in the conversion milestone; until then the
/// window runs against the mock so the whole shell is testable end to end.
pub fn default_converter() -> Arc<dyn Converter> {
    Arc::new(MockConverter::default())
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
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF document", &["pdf"])
            .set_title("Choose the scanned PDF")
            .pick_file()
        {
            self.state.select_input(path);
        }
    }

    fn pick_output(&mut self) {
        if self.state.output().is_none() {
            // Nothing selected yet; the output is chosen with the input.
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PDF document", &["pdf"])
            .set_title("Where should the searchable PDF be saved?");
        if let Some(name) = self.state.output().and_then(|path| path.file_name()) {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        if let Some(path) = dialog.save_file() {
            self.state.select_output(path);
        }
    }
}
