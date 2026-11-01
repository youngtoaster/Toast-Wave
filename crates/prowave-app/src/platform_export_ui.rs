//! UI for cross-platform DAW export & interchange.
//!
//! Enables exporting projects to:
//! - Apple Logic Pro X (`.logicx` package)
//! - Final Cut Pro XML (`.fcpxml`)
//! - Universal consolidated zero-aligned BWF stems (for Pro Tools, Ableton, FL Studio, Reaper, Cubase)
//! - Broadcast Wave stems with `bext` TimeReference timestamps
//! - Complete Universal Production Archive

use crate::model::Project;
use crate::project_io;
use egui::{Color32, RichText};
use prowave_project::PlatformExportFormat;
use rfd::FileDialog;
use std::path::{Path, PathBuf};

/// Commands queued by menu items for platform project imports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformImportCmd {
    LogicProX,
    FinalCutProXml,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlatformExportDialog {
    pub open: bool,
    pub format: PlatformExportFormat,
    pub target_path: String,
    pub status: Option<(bool /* is_err */, String)>,
}

impl Default for PlatformExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            format: PlatformExportFormat::BwfStemsConsolidated,
            target_path: String::new(),
            status: None,
        }
    }
}

impl PlatformExportDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_dialog(&mut self, default_dir: Option<&Path>, project_name: &str) {
        self.open = true;
        self.status = None;
        if self.target_path.is_empty() {
            let base = default_dir
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            self.target_path = base.join(format!("{}_Export", project_name)).to_string_lossy().to_string();
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, project: &Project) {
        if !self.open {
            return;
        }

        let mut close_dialog = false;

        egui::Window::new("Export for Other DAWs & Platforms")
            .open(&mut self.open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -20.0))
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.heading("Select Target Platform / Format:");
                ui.add_space(6.0);

                for &fmt in &PlatformExportFormat::ALL {
                    let selected = self.format == fmt;
                    let resp = ui.radio(selected, fmt.display_name());
                    if resp.clicked() {
                        self.format = fmt;
                    }
                }

                ui.add_space(6.0);
                egui::Frame::canvas(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(self.format.description()).color(Color32::from_rgb(0xc0, 0xc0, 0xc0)).italics());
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Destination Path:");
                    ui.text_edit_singleline(&mut self.target_path);
                    if ui.button("Browse…").clicked() {
                        match self.format {
                            PlatformExportFormat::LogicProX => {
                                let def_name = format!("{}.logicx", project.name);
                                if let Some(p) = FileDialog::new()
                                    .set_file_name(def_name)
                                    .save_file()
                                {
                                    self.target_path = p.to_string_lossy().to_string();
                                }
                            }
                            PlatformExportFormat::FinalCutProXml => {
                                let def_name = format!("{}.fcpxml", project.name);
                                if let Some(p) = FileDialog::new()
                                    .set_file_name(def_name)
                                    .add_filter("Final Cut Pro XML", &["fcpxml"])
                                    .save_file()
                                {
                                    self.target_path = p.to_string_lossy().to_string();
                                }
                            }
                            PlatformExportFormat::BwfStemsConsolidated
                            | PlatformExportFormat::BwfStemsTimestamped
                            | PlatformExportFormat::UniversalBundle => {
                                if let Some(p) = FileDialog::new().pick_folder() {
                                    self.target_path = p.join(format!("{}_Export", project.name)).to_string_lossy().to_string();
                                }
                            }
                        }
                    }
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(format!("Project: {}", project.name));
                    ui.label(format!("|  BPM: {:.1}", project.tempo));
                    ui.label(format!("|  Sig: {}/{}", project.sig.0, project.sig.1));
                    ui.label(format!("|  Tracks: {}", project.tracks.len()));
                });

                if let Some((is_err, ref msg)) = self.status {
                    ui.add_space(8.0);
                    let color = if is_err {
                        Color32::from_rgb(0xff, 0x60, 0x60)
                    } else {
                        Color32::from_rgb(0x60, 0xd0, 0x60)
                    };
                    ui.colored_label(color, msg);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_dialog = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let export_btn = egui::Button::new(
                            RichText::new("Export Project")
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(0x2f, 0x6f, 0xd8));

                        if ui.add(export_btn).clicked() {
                            let out_path = PathBuf::from(self.target_path.trim());
                            if out_path.as_os_str().is_empty() {
                                self.status = Some((true, "Please choose a destination path.".into()));
                            } else {
                                let (doc, media) = project_io::to_doc(project);
                                match prowave_project::export_project_for_platform(
                                    &doc,
                                    &media,
                                    &out_path,
                                    self.format,
                                ) {
                                    Ok(res_path) => {
                                        self.status = Some((
                                            false,
                                            format!("Successfully exported to {}", res_path.display()),
                                        ));
                                    }
                                    Err(e) => {
                                        self.status = Some((
                                            true,
                                            format!("Export failed: {e}"),
                                        ));
                                    }
                                }
                            }
                        }
                    });
                });
            });

        if close_dialog {
            self.open = false;
        }
    }
}
