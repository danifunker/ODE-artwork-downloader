//! Main application state and UI implementation

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::api::{open_in_browser, ArtworkSearchQuery, SearchConfig, ContentType};
use crate::disc::{supported_extensions, parse_filename, ConfidenceLevel, DiscInfo, DiscReader, DiscFormat, FilesystemType};
use crate::export::{export_artwork, export_artwork_from_url, generate_output_path, ExportResult, ExportSettings};
use crate::search::ImageResult;
use crate::update::{UpdateConfig, UpdateInfo};

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
    /// Whether to show the log window
    show_log_window: bool,
    /// Last preview error message
    preview_error: Option<String>,
    /// Update configuration
    update_config: UpdateConfig,
    /// Update information receiver
    update_receiver: Option<Receiver<Result<UpdateInfo, String>>>,
    /// Latest update info
    update_info: Option<UpdateInfo>,
    /// Whether to show update notification
    show_update_notification: bool,
    /// Whether update check has been performed
    update_check_done: bool,
    /// Search configuration
    search_config: SearchConfig,
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
            show_log_window: false,
            preview_error: None,
            update_config: UpdateConfig::load(),
            update_receiver: None,
            update_info: None,
            show_update_notification: false,
            update_check_done: false,
            search_config: SearchConfig::default(),
        }
    }

}

impl App {
    /// Triggers a search based on the current application state
    fn trigger_search(&mut self) {
        let use_musicbrainz = self.search_config.content_type == ContentType::AudioCDs
            && self.disc_info.as_ref().and_then(|r| r.as_ref().ok()).and_then(|i| i.toc.as_ref()).is_some();

        if use_musicbrainz {
            if let Some(disc_id) = self.disc_info.as_ref()
                .and_then(|r| r.as_ref().ok())
                .and_then(|i| i.toc.as_ref())
                .map(|toc| toc.calculate_musicbrainz_id())
            {
                self.log(LogLevel::Info, format!("Searching MusicBrainz for disc ID: {}", disc_id));
                let toc_string = self.disc_info.as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|i| i.toc.as_ref())
                    .map(|toc| toc.to_toc_string());
                self.start_musicbrainz_search(&disc_id, toc_string);
            } else {
                self.log(LogLevel::Error, "No disc ID available for MusicBrainz search");
            }
        } else {
            let query_for_search = self.search_query_text.clone();
            if !query_for_search.is_empty() {
                self.log(LogLevel::Info, format!("Searching: {}", query_for_search));
                self.start_search(&query_for_search);
            }
        }
    }

    /// Create a new App instance
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::default();
        
        // Start update check in background if enabled
        if app.update_config.update_check.enabled {
            app.start_update_check();
        }
        
        app
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

        // Store log messages in a vector to add after disc reading
        let log_messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log_messages_clone = log_messages.clone();
        
        // Set up logging callback to capture disc reading logs
        crate::disc::set_log_callback(std::sync::Arc::new(std::sync::Mutex::new(
            move |msg: String| {
                if let Ok(mut messages) = log_messages_clone.lock() {
                    messages.push(msg);
                }
            }
        )));

        match DiscReader::read(&path) {
            Ok(info) => {
                // Add collected log messages
                if let Ok(messages) = log_messages.lock() {
                    for msg in messages.iter() {
                        self.log(LogLevel::Info, msg.clone());
                    }
                }
                
                self.log(
                    LogLevel::Success,
                    format!("Successfully read disc: {}", info.title),
                );
                self.disc_info = Some(Ok(info));
            }
            Err(e) => {
                // Add collected log messages even on error
                if let Ok(messages) = log_messages.lock() {
                    for msg in messages.iter() {
                        self.log(LogLevel::Info, msg.clone());
                    }
                }
                
                let error_str = e.to_string();
                // Check if this is likely an HFS/HFS+ disc or other non-ISO9660 format
                let is_filesystem_error = error_str.contains("Primary Volume Descriptor")
                    || error_str.contains("HFS")
                    || error_str.contains("Parse error");

                if is_filesystem_error {
                    // Fall back to filename-only parsing for HFS and other unsupported filesystems
                    self.log(
                        LogLevel::Warning,
                        format!("Could not read disc structure (may be HFS/Mac format), using filename only"),
                    );

                    // Create a minimal DiscInfo from filename
                    let parsed = parse_filename(&path);
                    let format = DiscFormat::from_path(&path).unwrap_or(DiscFormat::Iso);

                    let fallback_info = DiscInfo {
                        path: path.clone(),
                        format,
                        filesystem: FilesystemType::Unknown,
                        volume_label: None,
                        title: parsed.title.clone(),
                        parsed_filename: parsed,
                        confidence: ConfidenceLevel::Low,
                        pvd: None,
                        toc: None,
                        hfs_mdb: None,
                        hfsplus_header: None,
                    };

                    self.disc_info = Some(Ok(fallback_info));
                } else {
                    self.log(LogLevel::Error, format!("Error reading disc: {}", e));
                    self.disc_info = Some(Err(error_str));
                }
            }
        }
        
        // Clear the log callback after processing
        crate::disc::clear_log_callback();

        // After processing, update the search query and trigger an automatic search
        if self.disc_info.as_ref().and_then(|r| r.as_ref().ok()).is_some() {
            self.update_search_query_from_disc();
            self.trigger_search();
        }
    }

    /// Open file picker dialog
    fn open_file_picker(&mut self) {
        let extensions: Vec<&str> = supported_extensions();

        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Disc Images", &extensions)
            .add_filter("ISO/Toast Files", &["iso", "toast"])
            .add_filter("CHD Files", &["chd"])
            .add_filter("BIN/CUE Files", &["bin", "cue"])
            .add_filter("All Files", &["*"])
            .pick_file()
        {
            self.process_file(path);
        }
    }

    /// Update search query from disc info using current config
    fn update_search_query_from_disc(&mut self) {
        if let Some(Ok(info)) = &self.disc_info {
            let search_query = ArtworkSearchQuery::from_disc_info_with_config(info, &self.search_config);
            self.search_query_text = search_query.build_query();
        }
    }

    /// Save search configuration to config.json
    fn save_search_config(&self) {
        if let Ok(config_str) = std::fs::read_to_string("config.json") {
            if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&config_str) {
                // Update the content_type field
                if let Some(search) = json.get_mut("search") {
                    if let Some(obj) = search.as_object_mut() {
                        obj.insert(
                            "content_type".to_string(),
                            serde_json::Value::String(self.search_config.content_type.as_str().to_string())
                        );
                        
                        // Write back to file
                        if let Ok(updated) = serde_json::to_string_pretty(&json) {
                            let _ = std::fs::write("config.json", updated);
                        }
                    }
                }
            }
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

    /// Start an async MusicBrainz search using disc ID
    fn start_musicbrainz_search(&mut self, disc_id: &str, toc_string: Option<String>) {
        let disc_id = disc_id.to_string();
        let (tx, rx) = mpsc::channel();

        self.search_in_progress = true;
        self.search_results.clear();
        self.selected_image_index = None;
        self.search_receiver = Some(rx);

        thread::spawn(move || {
            // Query MusicBrainz for releases
            let mb_results = crate::api::search_by_discid(&disc_id, toc_string.as_deref());
            
            let result = mb_results.and_then(|releases| {
                let mut all_results = Vec::new();
                
                // Convert MusicBrainz results to ImageResult format
                let mb_images: Vec<_> = releases.iter()
                    .filter_map(|release| {
                        // Prefer cover art URL, fall back to thumbnail
                        let image_url = release.cover_art_url.clone().or_else(|| release.thumbnail_url.clone())?;
                        let thumbnail = release.thumbnail_url.clone().unwrap_or_else(|| image_url.clone());
                        
                        let title = if let Some(ref date) = release.date {
                            format!("{} - {} ({})", release.artist, release.title, date)
                        } else {
                            format!("{} - {}", release.artist, release.title)
                        };

                        Some(crate::search::ImageResult {
                            image_url,
                            thumbnail_url: thumbnail,
                            title,
                            source: format!("MusicBrainz ({})", release.release_id),
                            width: None,
                            height: None,
                        })
                    })
                    .collect();
                
                all_results.extend(mb_images);
                
                // If we got at least one MusicBrainz result, search Discogs for the album
                if let Some(first_release) = releases.first() {
                    let search_query = format!("site:discogs.com {} {}", first_release.artist, first_release.title);
                    log::info!("Searching Discogs for album: {} - {}", first_release.artist, first_release.title);
                    
                    // Search specifically on Discogs
                    match crate::search::search_images(&search_query, 20) {
                        Ok(mut discogs_results) => {
                            // Mark Discogs results with their source
                            for result in &mut discogs_results {
                                if result.source.contains("discogs.com") {
                                    result.source = format!("Discogs");
                                }
                            }
                            all_results.extend(discogs_results);
                        }
                        Err(e) => {
                            log::warn!("Failed to search Discogs: {}", e);
                        }
                    }
                }
                
                Ok(all_results)
            });
            
            let _ = tx.send(result);
        });
    }

    /// Poll for search results
    fn poll_search(&mut self) {
        if let Some(ref receiver) = self.search_receiver {
            match receiver.try_recv() {
                Ok(Ok(mut results)) => {
                    let count = results.len();
                    
                    // Count MusicBrainz vs Discogs results
                    let mb_count = results.iter().filter(|r| r.source.starts_with("MusicBrainz")).count();
                    let discogs_count = results.iter().filter(|r| r.source == "Discogs").count();
                    
                    // Sort: MusicBrainz results first, then web results by aspect ratio
                    results.sort_by(|a, b| {
                        // MusicBrainz results always come first
                        let a_is_mb = a.source.starts_with("MusicBrainz");
                        let b_is_mb = b.source.starts_with("MusicBrainz");
                        
                        if a_is_mb != b_is_mb {
                            return if a_is_mb { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
                        }
                        
                        // Within same category, sort by aspect ratio (closest to 1.0)
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
                    
                    let msg = if mb_count > 0 && discogs_count > 0 {
                        format!("Found {} MusicBrainz releases + {} Discogs images", mb_count, discogs_count)
                    } else if mb_count > 0 {
                        format!("Found {} MusicBrainz releases", mb_count)
                    } else {
                        format!("Found {} images", count)
                    };
                    self.log(LogLevel::Success, msg);
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
    #[allow(dead_code)]
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
        self.preview_error = None;
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
                            self.preview_error = None;
                            self.log(LogLevel::Success, "Preview loaded");
                        }
                        Err(e) => {
                            let msg = format!("Failed to decode image: {}", e);
                            self.preview_error = Some(msg.clone());
                            self.log(LogLevel::Error, msg);
                        }
                    }
                }
                Ok(Err(e)) => {
                    self.preview_loading = false;
                    self.preview_receiver = None;
                    let msg = format!("Failed to load: {}", e);
                    self.preview_error = Some(msg.clone());
                    self.log(LogLevel::Error, msg);
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

    /// Convert a local image file (for drag-and-drop artwork)
    fn convert_local_image(&mut self, image_path: &std::path::Path, output_path: &str) {
        let image_path = image_path.to_path_buf();
        let output = output_path.to_string();
        let (tx, rx) = mpsc::channel();

        self.export_in_progress = true;
        self.export_receiver = Some(rx);

        self.log(LogLevel::Info, format!("Converting to {}", output));

        thread::spawn(move || {
            // Read the local file
            let result = std::fs::read(&image_path)
                .map_err(|e| format!("Failed to read file: {}", e))
                .and_then(|bytes| {
                    let settings = ExportSettings::default();
                    export_artwork(&bytes, &output, &settings)
                });
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

    /// Start checking for updates
    fn start_update_check(&mut self) {
        if self.update_check_done {
            return;
        }

        let config = self.update_config.update_check.clone();
        let current_version = env!("APP_VERSION").to_string();
        let (tx, rx) = mpsc::channel();

        self.update_receiver = Some(rx);

        thread::spawn(move || {
            let result = crate::update::check_for_updates(&config, &current_version)
                .map_err(|e: Box<dyn std::error::Error>| e.to_string());
            let _ = tx.send(result);
        });
    }

    /// Poll for update check results
    fn poll_update_check(&mut self) {
        if let Some(ref receiver) = self.update_receiver {
            match receiver.try_recv() {
                Ok(Ok(info)) => {
                    self.update_check_done = true;
                    self.update_receiver = None;
                    
                    if info.is_outdated {
                        self.update_info = Some(info.clone());
                        self.show_update_notification = true;
                        self.log(
                            LogLevel::Info,
                            format!("Update available: v{} → v{}", info.current_version, info.latest_version)
                        );
                    }
                }
                Ok(Err(_e)) => {
                    self.update_check_done = true;
                    self.update_receiver = None;
                    // Silently fail update checks - don't spam users with errors
                }
                Err(TryRecvError::Empty) => {
                    // Still checking
                }
                Err(TryRecvError::Disconnected) => {
                    self.update_check_done = true;
                    self.update_receiver = None;
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

        // Poll for update check
        self.poll_update_check();

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
                // Check if it's an image file (for manual artwork drop)
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();

                let is_image = matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp");

                if is_image {
                    // It's an image - convert and save if we have a disc selected
                    if let Some(ref disc_path) = self.selected_path {
                        let output_path = generate_output_path(disc_path);
                        self.log(LogLevel::Info, format!("Converting dropped image: {}", path.display()));
                        self.convert_local_image(&path, &output_path);
                    } else {
                        self.log(LogLevel::Warning, "Drop a disc image first, then drop artwork to convert");
                    }
                } else {
                    // It's a disc image
                    self.process_file(path);
                }
            }
        }

        // Top panel with title
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("ODE Artwork Downloader");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("v{}", env!("APP_VERSION")));
                    ui.separator();
                    let log_count = self.log_messages.len();
                    let log_btn_text = if log_count > 0 {
                        format!("Log ({})", log_count)
                    } else {
                        "Log".to_string()
                    };
                    if ui.button(log_btn_text).clicked() {
                        self.show_log_window = !self.show_log_window;
                    }
                });
            });
            ui.add_space(4.0);
        });

        // Log window (separate window, hidden by default)
        if self.show_log_window {
            egui::Window::new("Log")
                .open(&mut self.show_log_window)
                .default_size([500.0, 300.0])
                .resizable(true)
                .show(ctx, |ui| {
                    if ui.button("Clear").clicked() {
                        self.log_messages.clear();
                    }
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
        }

        // Main central panel
        egui::CentralPanel::default().show(ctx, |ui| {
            // Top section with File Selection and Search Settings in columns
            ui.columns(2, |columns| {
                
                // --- Left Column: File Selection ---
                columns[0].group(|ui| {
                    ui.set_min_height(120.0);
                    ui.heading("File Selection");
                    ui.add_space(8.0);

                    if ui.button("Browse...").clicked() {
                        self.open_file_picker();
                    }

                    ui.add_space(8.0);

                    if let Some(ref path) = self.selected_path {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(path.display().to_string()).size(11.0)
                            ).wrap()
                        );
                    } else {
                        ui.colored_label(egui::Color32::GRAY, "No file selected (drag & drop supported)");
                    }
                });

                // --- Right Column: Search Settings ---
                columns[1].group(|ui| {
                    ui.set_min_height(120.0);
                    ui.heading("Search Settings");
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("Content Type:");
                        let mut changed = false;
                        egui::ComboBox::new("content_type_combo", "")
                            .selected_text(self.search_config.content_type.display_name())
                            .show_ui(ui, |ui| {
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::Any, "Any").clicked() { 
                                    changed = true; 
                                }
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::Games, "Games").clicked() { 
                                    changed = true; 
                                }
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::AppsUtilities, "Apps & Utilities").clicked() { 
                                    changed = true; 
                                }
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::AudioCDs, "Audio CDs").clicked() { 
                                    changed = true; 
                                }
                            });

                        if changed {
                            self.save_search_config();
                            self.update_search_query_from_disc();
                        }
                    });
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

                                // TOC information (for audio CDs)
                                if let Some(ref toc) = info.toc {
                                    ui.label("Audio Tracks:");
                                    ui.label(format!("{}", toc.track_count()));
                                    ui.end_row();

                                    ui.label("Total Length:");
                                    ui.label(toc.total_time_string());
                                    ui.end_row();

                                    ui.label("MusicBrainz ID:");
                                    let disc_id = toc.calculate_musicbrainz_id();
                                    ui.horizontal(|ui| {
                                        ui.label(&disc_id);
                                        if ui.small_button("📋").on_hover_text("Copy to clipboard").clicked() {
                                            ui.output_mut(|o| o.copied_text = disc_id.clone());
                                        }
                                        if ui.small_button("🔍").on_hover_text("Search on MusicBrainz").clicked() {
                                            let url = format!("https://musicbrainz.org/cdtoc/{}", disc_id);
                                            let _ = crate::api::open_in_browser(&url);
                                        }
                                    });
                                    ui.end_row();

                                    ui.label("FreeDB ID:");
                                    let freedb_id = toc.calculate_freedb_id();
                                    ui.horizontal(|ui| {
                                        ui.label(&freedb_id);
                                        if ui.small_button("📋").on_hover_text("Copy to clipboard").clicked() {
                                            ui.output_mut(|o| o.copied_text = freedb_id.clone());
                                        }
                                    });
                                    ui.end_row();
                                }

                                // HFS/HFS+ information
                                if let Some(ref mdb) = info.hfs_mdb {
                                    ui.label("HFS Volume:");
                                    ui.label(&mdb.volume_name);
                                    ui.end_row();

                                    ui.label("Allocation Blocks:");
                                    ui.label(format!("{}", mdb.alloc_blocks));
                                    ui.end_row();

                                    ui.label("Block Size:");
                                    ui.label(format!("{} bytes", mdb.alloc_block_size));
                                    ui.end_row();

                                    ui.label("Files/Dirs:");
                                    ui.label(format!("{} / {}", mdb.root_file_count, mdb.root_dir_count));
                                    ui.end_row();
                                }

                                if let Some(ref header) = info.hfsplus_header {
                                    ui.label("HFS+ Version:");
                                    ui.label(format!("{}", header.version));
                                    ui.end_row();

                                    ui.label("Block Size:");
                                    ui.label(format!("{} bytes", header.block_size));
                                    ui.end_row();

                                    ui.label("Total Blocks:");
                                    ui.label(format!("{}", header.total_blocks));
                                    ui.end_row();

                                    ui.label("Free Blocks:");
                                    ui.label(format!("{} ({:.1}%)", 
                                        header.free_blocks,
                                        (header.free_blocks as f64 / header.total_blocks as f64) * 100.0
                                    ));
                                    ui.end_row();

                                    ui.label("Files/Folders:");
                                    ui.label(format!("{} / {}", header.file_count, header.folder_count));
                                    ui.end_row();

                                    let total_size = header.total_blocks as u64 * header.block_size as u64;
                                    let free_size = header.free_blocks as u64 * header.block_size as u64;
                                    ui.label("Total Size:");
                                    ui.label(format!("{:.2} GB", total_size as f64 / 1_073_741_824.0));
                                    ui.end_row();

                                    ui.label("Free Space:");
                                    ui.label(format!("{:.2} GB", free_size as f64 / 1_073_741_824.0));
                                    ui.end_row();
                                }
                            });

                        ui.add_space(16.0);

                        // Prepare search query from disc info (clone to avoid borrow issues)
                        let search_query = ArtworkSearchQuery::from_disc_info_with_config(info, &self.search_config);
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
                        let use_musicbrainz = self.search_config.content_type == ContentType::AudioCDs && info.toc.is_some();
                        let disc_id = info.toc.as_ref().map(|toc| toc.calculate_musicbrainz_id());

                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!self.search_in_progress, |ui| {
                                let search_label = if use_musicbrainz {
                                    "Search MusicBrainz"
                                } else {
                                    "Search"
                                };
                                
                                if ui.button(search_label).clicked() {
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
                            self.trigger_search();
                        }

                        if browser_clicked {
                            if use_musicbrainz {
                                if let Some(ref id) = disc_id {
                                    let url = format!("https://musicbrainz.org/cdtoc/{}", id);
                                    self.log(LogLevel::Info, "Opening MusicBrainz in browser".to_string());
                                    if let Err(e) = open_in_browser(&url) {
                                        self.log(LogLevel::Error, e);
                                    }
                                }
                            } else {
                                // Build browser URL from current query
                                let encoded = urlencoding::encode(&query_for_search);
                                let browser_url = format!("https://www.google.com/search?tbm=isch&tbs=iar:s&q={}", encoded);
                                self.log(LogLevel::Info, format!("Opening browser: {}", query_for_search));
                                if let Err(e) = open_in_browser(&browser_url) {
                                    self.log(LogLevel::Error, e);
                                }
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
                        let has_results = !self.search_results.is_empty();
                        if has_results {
                            ui.add_space(8.0);
                        }
                    }
                    Some(Err(ref error)) => {
                        ui.colored_label(egui::Color32::RED, format!("Error: {}", error));
                    }
                    None => {
                        // Center the drag-and-drop hint in available space
                        let available = ui.available_size();
                        ui.allocate_space(egui::vec2(0.0, available.y * 0.3));

                        ui.vertical_centered(|ui| {
                            ui.label("Select a disc image file to view information.");
                            ui.add_space(40.0);
                            ui.label(
                                egui::RichText::new("Drag and drop disc image files here")
                                    .size(18.0)
                                    .color(egui::Color32::GRAY)
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("Supported: ISO, Toast, CHD, BIN/CUE, MDS/MDF")
                                    .small()
                                    .color(egui::Color32::DARK_GRAY)
                            );
                        });
                    }
                }
            });

            // Search results section - outside the group so it can expand
            if !self.search_results.is_empty() {
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    // Left side: results list
                    ui.vertical(|ui| {
                        ui.heading("Search Results");
                        ui.label(format!("{} images - click to preview", self.search_results.len()));
                        ui.add_space(8.0);

                        // Fixed height for ~10 results with scrolling
                        let results_height = 280.0_f32.min(ui.available_height() - 50.0).max(150.0);
                        egui::ScrollArea::vertical()
                            .id_salt("search_results")
                            .auto_shrink([false, false])
                            .min_scrolled_height(results_height)
                            .max_height(results_height)
                            .max_width(420.0)
                            .show(ui, |ui| {
                                let mut clicked_idx = None;

                                for (idx, result) in self.search_results.iter().enumerate() {
                                    let is_selected = self.selected_image_index == Some(idx);
                                    let truncated_title = if result.title.chars().count() > 50 {
                                        format!("{}...", result.title.chars().take(50).collect::<String>())
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

                                    // Add hover text and context menu
                                    let response = response.on_hover_text(&result.image_url);
                                    response.context_menu(|ui| {
                                        if ui.button("Copy URL to clipboard").clicked() {
                                            ui.output_mut(|o| o.copied_text = result.image_url.clone());
                                            ui.close_menu();
                                        }
                                        if ui.button("Open in browser").clicked() {
                                            let _ = open_in_browser(&result.image_url);
                                            ui.close_menu();
                                        }
                                    });
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
                        } else if let Some(ref error) = self.preview_error {
                            // Show error message
                            ui.add_space(20.0);
                            ui.colored_label(egui::Color32::RED, error);
                            ui.add_space(10.0);
                            ui.label("Tip: Download the image manually and drop it here");

                            // Drop zone for manual image
                            let output_path = self.selected_path.as_ref()
                                .map(|p| generate_output_path(p));
                            if let Some(ref path) = output_path {
                                ui.add_space(10.0);
                                ui.label(egui::RichText::new(format!("Will save to: {}", path))
                                    .small()
                                    .color(egui::Color32::GRAY));
                            }
                        } else {
                            ui.add_space(50.0);
                            ui.label("Select an image to preview");
                        }
                    });
                });
            }
        });

        // Show drag-and-drop preview
        preview_files_being_dropped(ctx);

        // Show update notification dialog
        if self.show_update_notification {
            if let Some(update_info) = self.update_info.clone() {
                egui::Window::new("Update Available")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "A new version is available: v{}",
                            update_info.latest_version
                        ));
                        ui.label(format!("Current version: v{}", update_info.current_version));
                        ui.add_space(10.0);

                        ui.horizontal(|ui| {
                            if ui.button("Take me to the download").clicked() {
                                if let Err(e) = open_in_browser(&update_info.releases_url) {
                                    self.log(LogLevel::Error, format!("Failed to open browser: {}", e));
                                }
                                self.show_update_notification = false;
                            }
                            if ui.button("Skip").clicked() {
                                self.show_update_notification = false;
                            }
                        });
                    });
            }
        }
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
