//! Main application state and UI implementation

use eframe::egui;
use std::path::PathBuf;

use crate::disc::{supported_extensions, ConfidenceLevel, DiscInfo, DiscReader};

/// Main application state
pub struct App {
    /// Currently selected file path
    selected_path: Option<PathBuf>,
    /// Information about the selected disc
    disc_info: Option<Result<DiscInfo, String>>,
    /// Status/log messages
    log_messages: Vec<LogMessage>,
    /// Dropped files (for drag-and-drop)
    dropped_files: Vec<egui::DroppedFile>,
}

/// A log message with severity level
#[derive(Clone)]
struct LogMessage {
    text: String,
    level: LogLevel,
}

#[derive(Clone, Copy, PartialEq)]
enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl Default for App {
    fn default() -> Self {
        Self {
            selected_path: None,
            disc_info: None,
            log_messages: Vec::new(),
            dropped_files: Vec::new(),
        }
    }
}

impl App {
    /// Create a new App instance
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    /// Add a log message
    fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.log_messages.push(LogMessage {
            text: message.into(),
            level,
        });
        // Keep only last 100 messages
        if self.log_messages.len() > 100 {
            self.log_messages.remove(0);
        }
    }

    /// Process a selected file
    fn process_file(&mut self, path: PathBuf) {
        self.log(LogLevel::Info, format!("Processing: {}", path.display()));
        self.selected_path = Some(path.clone());

        match DiscReader::read(&path) {
            Ok(info) => {
                self.log(
                    LogLevel::Success,
                    format!("Successfully read disc: {}", info.title),
                );
                self.disc_info = Some(Ok(info));
            }
            Err(e) => {
                self.log(LogLevel::Error, format!("Error reading disc: {}", e));
                self.disc_info = Some(Err(e.to_string()));
            }
        }
    }

    /// Open file picker dialog
    fn open_file_picker(&mut self) {
        let extensions: Vec<&str> = supported_extensions();

        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Disc Images", &extensions)
            .add_filter("ISO Files", &["iso"])
            .add_filter("CHD Files", &["chd"])
            .add_filter("BIN/CUE Files", &["bin", "cue"])
            .add_filter("All Files", &["*"])
            .pick_file()
        {
            self.process_file(path);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Handle dropped files
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                self.dropped_files = i.raw.dropped_files.clone();
            }
        });

        // Process dropped files
        if let Some(file) = self.dropped_files.pop() {
            if let Some(path) = file.path {
                self.process_file(path);
            }
        }

        // Top panel with title
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("ODE Artwork Downloader");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("v0.1.0");
                });
            });
            ui.add_space(4.0);
        });

        // Bottom panel with log
        egui::TopBottomPanel::bottom("log_panel")
            .resizable(true)
            .min_height(100.0)
            .default_height(150.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label("Log");
                ui.separator();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for msg in &self.log_messages {
                            let color = match msg.level {
                                LogLevel::Info => egui::Color32::GRAY,
                                LogLevel::Success => egui::Color32::GREEN,
                                LogLevel::Warning => egui::Color32::YELLOW,
                                LogLevel::Error => egui::Color32::RED,
                            };
                            ui.colored_label(color, &msg.text);
                        }
                    });
            });

        // Main central panel
        egui::CentralPanel::default().show(ctx, |ui| {
            // File selection section
            ui.group(|ui| {
                ui.heading("File Selection");
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Browse...").clicked() {
                        self.open_file_picker();
                    }

                    if let Some(ref path) = self.selected_path {
                        ui.label(path.display().to_string());
                    } else {
                        ui.label("No file selected (drag & drop supported)");
                    }
                });
            });

            ui.add_space(16.0);

            // Disc information section
            ui.group(|ui| {
                ui.heading("Disc Information");
                ui.add_space(8.0);

                match &self.disc_info {
                    Some(Ok(info)) => {
                        egui::Grid::new("disc_info_grid")
                            .num_columns(2)
                            .spacing([40.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("Volume Label:");
                                if let Some(ref label) = info.volume_label {
                                    ui.strong(label);
                                } else {
                                    ui.colored_label(egui::Color32::GRAY, "Not found");
                                }
                                ui.end_row();

                                ui.label("Format:");
                                ui.label(info.format.display_name());
                                ui.end_row();

                                ui.label("Filesystem:");
                                ui.label(info.filesystem.display_name());
                                ui.end_row();

                                ui.label("Confidence:");
                                let (color, text) = match info.confidence {
                                    ConfidenceLevel::High => {
                                        (egui::Color32::GREEN, info.confidence.display_name())
                                    }
                                    ConfidenceLevel::Medium => {
                                        (egui::Color32::YELLOW, info.confidence.display_name())
                                    }
                                    ConfidenceLevel::Low => {
                                        (egui::Color32::LIGHT_RED, info.confidence.display_name())
                                    }
                                };
                                ui.colored_label(color, text);
                                ui.end_row();

                                // Parsed filename info
                                ui.label("Parsed Title:");
                                ui.label(&info.parsed_filename.title);
                                ui.end_row();

                                if let Some(ref region) = info.parsed_filename.region {
                                    ui.label("Region:");
                                    ui.label(region);
                                    ui.end_row();
                                }

                                if let Some(disc) = info.parsed_filename.disc_number {
                                    ui.label("Disc Number:");
                                    ui.label(format!("{}", disc));
                                    ui.end_row();
                                }

                                if let Some(ref serial) = info.parsed_filename.serial {
                                    ui.label("Serial:");
                                    ui.label(serial);
                                    ui.end_row();
                                }

                                // Cover art status
                                ui.label("Cover Art:");
                                if info.has_cover_art() {
                                    ui.colored_label(egui::Color32::GREEN, "Found");
                                } else {
                                    ui.colored_label(egui::Color32::LIGHT_RED, "Not found");
                                }
                                ui.end_row();
                            });

                        ui.add_space(16.0);

                        // Action buttons
                        ui.horizontal(|ui| {
                            if ui.button("Search Cover Art").clicked() {
                                self.log(
                                    LogLevel::Warning,
                                    "Cover art search not yet implemented",
                                );
                            }

                            if ui.button("Download Selected").clicked() {
                                self.log(LogLevel::Warning, "Download not yet implemented");
                            }
                        });
                    }
                    Some(Err(ref error)) => {
                        ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                    }
                    None => {
                        ui.label("Select a disc image file to view information.");

                        // Show drag-and-drop hint
                        ui.add_space(20.0);
                        let rect = ui.available_rect_before_wrap();
                        let response = ui.allocate_rect(rect, egui::Sense::hover());

                        if response.hovered() {
                            ui.painter().rect_stroke(
                                rect,
                                8.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 100, 200)),
                            );
                        }

                        ui.centered_and_justified(|ui| {
                            ui.label("Drag and drop disc image files here");
                        });
                    }
                }
            });
        });

        // Show drag-and-drop preview
        preview_files_being_dropped(ctx);
    }
}

/// Preview files being dragged over the window
fn preview_files_being_dropped(ctx: &egui::Context) {
    use egui::{Align2, Color32, Id, LayerId, Order, TextStyle};

    if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
        let painter =
            ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("file_drop_target")));

        let screen_rect = ctx.screen_rect();
        painter.rect_filled(screen_rect, 0.0, Color32::from_black_alpha(192));
        painter.text(
            screen_rect.center(),
            Align2::CENTER_CENTER,
            "Drop disc image to scan",
            TextStyle::Heading.resolve(&ctx.style()),
            Color32::WHITE,
        );
    }
}
