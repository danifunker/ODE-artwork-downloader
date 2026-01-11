//! Main application state and UI implementation

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::api::{open_in_browser, ArtworkSearchQuery};
use crate::disc::{supported_extensions, ConfidenceLevel, DiscInfo, DiscReader};
use crate::export::{export_artwork_from_url, generate_output_path, ExportResult, ExportSettings};
use crate::search::ImageResult;

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
    /// Search results from image search
    search_results: Vec<ImageResult>,
    /// Receiver for async search results
    search_receiver: Option<Receiver<Result<Vec<ImageResult>, String>>>,
    /// Currently selected image index
    selected_image_index: Option<usize>,
    /// Is a search in progress?
    search_in_progress: bool,
    /// Preview image texture
    preview_texture: Option<egui::TextureHandle>,
    /// Receiver for preview image data
    preview_receiver: Option<Receiver<Result<Vec<u8>, String>>>,
    /// Is preview loading?
    preview_loading: bool,
    /// URL of the currently loaded preview (to avoid reloading)
    preview_url: Option<String>,
    /// Receiver for export results
    export_receiver: Option<Receiver<Result<ExportResult, String>>>,
    /// Is export in progress?
    export_in_progress: bool,
    /// Editable search query string
    search_query_text: String,
    /// Manual URL override for downloading
    manual_url: String,
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
            search_results: Vec::new(),
            search_receiver: None,
            selected_image_index: None,
            search_in_progress: false,
            preview_texture: None,
            preview_receiver: None,
            preview_loading: false,
            preview_url: None,
            export_receiver: None,
            export_in_progress: false,
            search_query_text: String::new(),
            manual_url: String::new(),
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

        // Clear previous search state
        self.search_query_text.clear();
        self.manual_url.clear();
        self.search_results.clear();
        self.selected_image_index = None;
        self.preview_texture = None;
        self.preview_url = None;

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

    /// Start an async image search
    fn start_search(&mut self, query: &str) {
        let query = query.to_string();
        let (tx, rx) = mpsc::channel();

        self.search_in_progress = true;
        self.search_results.clear();
        self.selected_image_index = None;
        self.search_receiver = Some(rx);

        thread::spawn(move || {
            let result = crate::search::search_images(&query, 20);
            let _ = tx.send(result);
        });
    }

    /// Poll for search results
    fn poll_search(&mut self) {
        if let Some(ref receiver) = self.search_receiver {
            match receiver.try_recv() {
                Ok(Ok(mut results)) => {
                    let count = results.len();
                    // Sort by aspect ratio - closest to 1.0 (square) first
                    results.sort_by(|a, b| {
                        let aspect_a = match (a.width, a.height) {
                            (Some(w), Some(h)) if h > 0 => {
                                let ratio = w as f64 / h as f64;
                                (ratio - 1.0).abs()
                            }
                            _ => f64::MAX, // Unknown dimensions go to end
                        };
                        let aspect_b = match (b.width, b.height) {
                            (Some(w), Some(h)) if h > 0 => {
                                let ratio = w as f64 / h as f64;
                                (ratio - 1.0).abs()
                            }
                            _ => f64::MAX,
                        };
                        aspect_a.partial_cmp(&aspect_b).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    self.search_results = results;
                    self.search_in_progress = false;
                    self.search_receiver = None;
                    self.log(LogLevel::Success, format!("Found {} images (sorted by aspect ratio)", count));
                }
                Ok(Err(e)) => {
                    self.search_in_progress = false;
                    self.search_receiver = None;
                    self.log(LogLevel::Error, format!("Search failed: {}", e));
                }
                Err(TryRecvError::Empty) => {
                    // Still searching, keep waiting
                }
                Err(TryRecvError::Disconnected) => {
                    self.search_in_progress = false;
                    self.search_receiver = None;
                    self.log(LogLevel::Error, "Search thread terminated unexpectedly");
                }
            }
        }
    }

    /// Get the currently selected image URL
    fn selected_image_url(&self) -> Option<&str> {
        self.selected_image_index
            .and_then(|i| self.search_results.get(i))
            .map(|r| r.image_url.as_str())
    }

    /// Start loading a preview image
    fn load_preview(&mut self, url: &str) {
        // Don't reload if already loading this URL
        if self.preview_url.as_deref() == Some(url) && (self.preview_loading || self.preview_texture.is_some()) {
            return;
        }

        let url = url.to_string();
        let (tx, rx) = mpsc::channel();

        self.preview_loading = true;
        self.preview_url = Some(url.clone());
        self.preview_texture = None;
        self.preview_receiver = Some(rx);

        thread::spawn(move || {
            let result = fetch_image_bytes(&url);
            let _ = tx.send(result);
        });
    }

    /// Poll for preview image data
    fn poll_preview(&mut self, ctx: &egui::Context) {
        if let Some(ref receiver) = self.preview_receiver {
            match receiver.try_recv() {
                Ok(Ok(bytes)) => {
                    self.preview_loading = false;
                    self.preview_receiver = None;

                    // Convert bytes to texture
                    match load_image_from_bytes(&bytes) {
                        Ok(color_image) => {
                            let texture = ctx.load_texture(
                                "preview",
                                color_image,
                                egui::TextureOptions::LINEAR,
                            );
                            self.preview_texture = Some(texture);
                            self.log(LogLevel::Success, "Preview loaded");
                        }
                        Err(e) => {
                            self.log(LogLevel::Error, format!("Failed to decode image: {}", e));
                        }
                    }
                }
                Ok(Err(e)) => {
                    self.preview_loading = false;
                    self.preview_receiver = None;
                    self.log(LogLevel::Error, format!("Failed to load preview: {}", e));
                }
                Err(TryRecvError::Empty) => {
                    // Still loading
                }
                Err(TryRecvError::Disconnected) => {
                    self.preview_loading = false;
                    self.preview_receiver = None;
                }
            }
        }
    }

    /// Start exporting artwork
    fn start_export(&mut self, image_url: &str, output_path: &str) {
        let url = image_url.to_string();
        let path = output_path.to_string();
        let (tx, rx) = mpsc::channel();

        self.export_in_progress = true;
        self.export_receiver = Some(rx);

        self.log(LogLevel::Info, format!("Downloading and converting to {}", path));

        thread::spawn(move || {
            let settings = ExportSettings::default();
            let result = export_artwork_from_url(&url, &path, &settings);
            let _ = tx.send(result);
        });
    }

    /// Poll for export results
    fn poll_export(&mut self) {
        if let Some(ref receiver) = self.export_receiver {
            match receiver.try_recv() {
                Ok(Ok(result)) => {
                    self.export_in_progress = false;
                    self.export_receiver = None;
                    let msg = if result.was_cropped {
                        format!(
                            "Saved to {} (cropped from {}x{} to {}x{})",
                            result.output_path,
                            result.original_size.0,
                            result.original_size.1,
                            result.final_size.0,
                            result.final_size.1
                        )
                    } else {
                        format!(
                            "Saved to {} ({}x{})",
                            result.output_path,
                            result.final_size.0,
                            result.final_size.1
                        )
                    };
                    self.log(LogLevel::Success, msg);
                }
                Ok(Err(e)) => {
                    self.export_in_progress = false;
                    self.export_receiver = None;
                    self.log(LogLevel::Error, format!("Export failed: {}", e));
                }
                Err(TryRecvError::Empty) => {
                    // Still exporting
                }
                Err(TryRecvError::Disconnected) => {
                    self.export_in_progress = false;
                    self.export_receiver = None;
                    self.log(LogLevel::Error, "Export thread terminated unexpectedly");
                }
            }
        }
    }
}

/// Fetch image bytes from a URL
fn fetch_image_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to fetch image: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    response
        .bytes()
        .map(|b| b.to_vec())
        .map_err(|e| format!("Failed to read image bytes: {}", e))
}

/// Load image from bytes into egui ColorImage
fn load_image_from_bytes(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let image = image::load_from_memory(bytes)
        .map_err(|e| format!("Failed to decode image: {}", e))?;

    let size = [image.width() as usize, image.height() as usize];
    let image_buffer = image.to_rgba8();
    let pixels = image_buffer.as_flat_samples();

    Ok(egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_slice()))
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for search results
        self.poll_search();

        // Poll for preview image
        self.poll_preview(ctx);

        // Poll for export results
        self.poll_export();

        // Request repaint while loading
        if self.search_in_progress || self.preview_loading || self.export_in_progress {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

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

                        // Prepare search query from disc info (clone to avoid borrow issues)
                        let search_query = ArtworkSearchQuery::from_disc_info(info);
                        let default_query = search_query.build_query();

                        // Initialize search query text if empty
                        if self.search_query_text.is_empty() {
                            self.search_query_text = default_query.clone();
                        }

                        // Editable search query
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            ui.add(egui::TextEdit::singleline(&mut self.search_query_text)
                                .desired_width(400.0)
                                .hint_text("Enter search query..."));
                            if ui.button("Reset").clicked() {
                                self.search_query_text = default_query.clone();
                            }
                        });

                        ui.add_space(8.0);

                        // Action buttons
                        let mut search_clicked = false;
                        let mut browser_clicked = false;
                        let query_for_search = self.search_query_text.clone();

                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!self.search_in_progress, |ui| {
                                if ui.button("Search").clicked() {
                                    search_clicked = true;
                                }
                                if ui.button("Open in Browser").clicked() {
                                    browser_clicked = true;
                                }
                            });
                            if self.search_in_progress {
                                ui.spinner();
                                ui.label("Searching...");
                            }
                        });

                        if search_clicked {
                            self.log(LogLevel::Info, format!("Searching: {}", query_for_search));
                            self.start_search(&query_for_search);
                        }

                        if browser_clicked {
                            // Build browser URL from current query
                            let encoded = urlencoding::encode(&query_for_search);
                            let browser_url = format!("https://www.google.com/search?tbm=isch&tbs=iar:s&q={}", encoded);
                            self.log(LogLevel::Info, format!("Opening browser: {}", query_for_search));
                            if let Err(e) = open_in_browser(&browser_url) {
                                self.log(LogLevel::Error, e);
                            }
                        }

                        ui.add_space(8.0);

                        // Manual URL override
                        let mut manual_preview_clicked = false;
                        let preview_loading = self.preview_loading;

                        ui.horizontal(|ui| {
                            ui.label("Manual URL:");
                            let available_width = ui.available_width() - 80.0; // Reserve space for button
                            ui.add(egui::TextEdit::singleline(&mut self.manual_url)
                                .desired_width(available_width.max(200.0))
                                .hint_text("Paste image URL here..."));

                            let can_preview_manual = !self.manual_url.is_empty() && !preview_loading;

                            if ui.add_enabled(can_preview_manual, egui::Button::new("Preview")).clicked() {
                                manual_preview_clicked = true;
                            }
                        });

                        if manual_preview_clicked {
                            let url = self.manual_url.clone();
                            if !url.is_empty() {
                                self.load_preview(&url);
                            }
                        }

                        // Display search results
                        if !self.search_results.is_empty() {
                            ui.add_space(16.0);
                            ui.separator();

                            // Two-column layout: results list on left, preview on right
                            ui.horizontal(|ui| {
                                // Left side: results list
                                ui.vertical(|ui| {
                                    ui.heading("Search Results");
                                    ui.label(format!("{} images - click to preview", self.search_results.len()));
                                    ui.add_space(8.0);

                                    egui::ScrollArea::vertical()
                                        .id_salt("search_results")
                                        .max_height(250.0)
                                        .max_width(350.0)
                                        .show(ui, |ui| {
                                            let mut clicked_idx = None;

                                            for (idx, result) in self.search_results.iter().enumerate() {
                                                let is_selected = self.selected_image_index == Some(idx);
                                                let truncated_title = if result.title.chars().count() > 40 {
                                                    format!("{}...", result.title.chars().take(40).collect::<String>())
                                                } else {
                                                    result.title.clone()
                                                };
                                                let text = format!(
                                                    "{}. {} ({}x{})",
                                                    idx + 1,
                                                    truncated_title,
                                                    result.width.unwrap_or(0),
                                                    result.height.unwrap_or(0)
                                                );

                                                let response = ui.selectable_label(is_selected, &text);
                                                if response.clicked() {
                                                    clicked_idx = Some(idx);
                                                }
                                                response.on_hover_text(&result.image_url);
                                            }

                                            // Handle selection change
                                            if let Some(idx) = clicked_idx {
                                                self.selected_image_index = Some(idx);
                                                let url = self.search_results.get(idx).map(|r| r.image_url.clone());
                                                if let Some(url) = url {
                                                    self.load_preview(&url);
                                                }
                                            }
                                        });
                                });

                                ui.add_space(16.0);

                                // Right side: preview
                                ui.vertical(|ui| {
                                    ui.heading("Preview");

                                    if self.preview_loading {
                                        ui.add_space(50.0);
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label("Loading...");
                                        });
                                    } else if let Some(ref texture) = self.preview_texture {
                                        let size = texture.size_vec2();
                                        // Scale to fit in preview area (max 200x200)
                                        let max_size = 200.0;
                                        let scale = (max_size / size.x).min(max_size / size.y).min(1.0);
                                        let display_size = egui::vec2(size.x * scale, size.y * scale);

                                        // Clone data needed for closures before borrowing self
                                        let texture_id = texture.id();
                                        let img_width = size.x as u32;
                                        let img_height = size.y as u32;
                                        let can_download = self.selected_path.is_some()
                                            && self.preview_url.is_some()
                                            && !self.export_in_progress;
                                        let export_in_progress = self.export_in_progress;
                                        let output_path = self.selected_path.as_ref()
                                            .map(|p| generate_output_path(p));
                                        let preview_url = self.preview_url.clone();

                                        // Image and download button side by side
                                        let mut start_export_data: Option<(String, String)> = None;

                                        ui.horizontal(|ui| {
                                            // Image on the left
                                            ui.vertical(|ui| {
                                                ui.image((texture_id, display_size));
                                                // Show dimensions below image
                                                ui.label(format!("{}x{}", img_width, img_height));
                                            });

                                            ui.add_space(16.0);

                                            // Download controls on the right
                                            ui.vertical(|ui| {
                                                ui.add_enabled_ui(can_download, |ui| {
                                                    let btn_text = if export_in_progress {
                                                        "Downloading..."
                                                    } else {
                                                        "Download & Save"
                                                    };

                                                    if ui.button(btn_text).clicked() {
                                                        if let (Some(url), Some(path)) = (
                                                            preview_url.clone(),
                                                            output_path.clone(),
                                                        ) {
                                                            start_export_data = Some((url, path));
                                                        }
                                                    }
                                                });

                                                if export_in_progress {
                                                    ui.horizontal(|ui| {
                                                        ui.spinner();
                                                        ui.label("Converting...");
                                                    });
                                                }

                                                ui.add_space(8.0);

                                                // Show where it will be saved
                                                if let Some(ref path) = output_path {
                                                    ui.label(egui::RichText::new("Save to:")
                                                        .small()
                                                        .color(egui::Color32::GRAY));
                                                    ui.label(egui::RichText::new(path)
                                                        .small()
                                                        .color(egui::Color32::GRAY));
                                                }
                                            });
                                        });

                                        // Handle export after closures
                                        if let Some((url, path)) = start_export_data {
                                            self.start_export(&url, &path);
                                        }
                                    } else {
                                        ui.add_space(50.0);
                                        ui.label("Select an image to preview");
                                    }
                                });
                            });
                        }
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
