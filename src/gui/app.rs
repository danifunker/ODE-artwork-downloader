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

use super::browse_view::BrowseView;

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
    /// Whether to show the log settings dialog
    show_log_settings: bool,
    /// Whether to show the artwork search results / preview window
    show_search_window: bool,
    /// Current UI log level (one of error/warn/info/debug/trace/off)
    log_level: String,
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
    /// Receiver for log messages from the global logger
    global_log_receiver: Option<Receiver<String>>,
    /// Browse view for filesystem browsing
    browse_view: BrowseView,
    /// Whether to show the browse window
    show_browse_window: bool,
    /// Receiver for user agent capture result
    user_agent_receiver: Option<Receiver<Result<String, String>>>,
    /// Whether user agent capture is in progress
    user_agent_capture_in_progress: bool,
    /// Receiver for the background redump DB update job
    db_update_receiver: Option<Receiver<Result<crate::db::UpdateOutcome, String>>>,
    /// Whether the DB update has finished (success or failure)
    db_update_done: bool,
    /// Live progress of the current hashing job (shared with the worker thread)
    hash_progress: Option<std::sync::Arc<std::sync::Mutex<crate::disc::hasher::HashProgress>>>,
    /// Receiver for the hashing worker's final result
    hash_receiver: Option<Receiver<Result<crate::disc::hasher::TrackHashes, String>>>,
    /// Rolling rate/ETA estimator for the active hashing job
    hash_rate_tracker: super::progress::RateTracker,
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
            show_log_settings: false,
            show_search_window: false,
            log_level: crate::config::get_config().log_level.clone(),
            preview_error: None,
            update_config: UpdateConfig::load(),
            update_receiver: None,
            update_info: None,
            show_update_notification: false,
            update_check_done: false,
            search_config: SearchConfig::default(),
            global_log_receiver: None,
            browse_view: BrowseView::new(),
            show_browse_window: false,
            user_agent_receiver: None,
            user_agent_capture_in_progress: false,
            db_update_receiver: None,
            db_update_done: false,
            hash_progress: None,
            hash_receiver: None,
            hash_rate_tracker: super::progress::RateTracker::default(),
        }
    }
}

impl App {
    /// Create a new App instance
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::default();

        // Take the log receiver from the global storage (set in main.rs)
        app.global_log_receiver = super::take_log_receiver();

        // Start update check in background if enabled
        if app.update_config.update_check.enabled {
            app.start_update_check();
        }

        // Refresh the redump lookup DB in the background.
        app.start_db_update();

        app
    }

    /// Spawn a background thread to check for / download the latest redump DB.
    fn start_db_update(&mut self) {
        if self.db_update_done || self.db_update_receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.db_update_receiver = Some(rx);
        thread::spawn(move || {
            let result = match crate::db::DatabaseManager::new() {
                Ok(mgr) => mgr.update_if_needed().map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(result);
        });
    }

    /// Run the redump lookup cascade against the current disc info.
    /// Attaches matches in-place and writes a one-line log entry. Failures
    /// (missing DB, query error) are logged at debug level and treated as
    /// "no match" — the rest of the flow continues unchanged.
    fn enrich_with_redump(&mut self, info: &mut DiscInfo) {
        let mgr = match crate::db::DatabaseManager::new() {
            Ok(m) => m,
            Err(e) => {
                log::debug!("Redump lookup skipped (manager init): {e}");
                return;
            }
        };
        let conn = match mgr.open() {
            Ok(c) => c,
            Err(crate::db::manager::DbError::NotInstalled) => {
                log::debug!("Redump lookup skipped: DB not yet downloaded");
                return;
            }
            Err(e) => {
                log::warn!("Redump lookup skipped: {e}");
                return;
            }
        };
        match crate::db::cascade_from_disc(&conn, info) {
            Ok(matches) => {
                if matches.is_empty() {
                    self.log(LogLevel::Info, "Redump: no match");
                } else {
                    let head = &matches[0];
                    let more = matches.len().saturating_sub(1);
                    let suffix = if more > 0 {
                        format!(" (+{more} more)")
                    } else {
                        String::new()
                    };
                    self.log(
                        LogLevel::Success,
                        format!(
                            "Redump match via {:?}: {} [#{}]{}",
                            head.matched_via, head.title, head.redump_id, suffix
                        ),
                    );
                }
                info.redump_matches = Some(matches);
            }
            Err(e) => {
                log::warn!("Redump cascade failed: {e}");
            }
        }
    }

    /// Spawn the hashing worker for the currently loaded disc.
    fn start_hashing(&mut self) {
        // Cancel any in-flight hasher first; user just loaded a new disc.
        self.cancel_hashing();

        let Some(Ok(info)) = self.disc_info.as_ref() else {
            return;
        };
        let info = info.clone();
        let progress = std::sync::Arc::new(std::sync::Mutex::new(
            crate::disc::hasher::HashProgress::default(),
        ));
        let (tx, rx) = mpsc::channel();
        self.hash_progress = Some(progress.clone());
        self.hash_receiver = Some(rx);
        self.hash_rate_tracker.reset();

        thread::spawn(move || {
            let result = crate::disc::hasher::hash_data_track(&info, progress)
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    fn cancel_hashing(&mut self) {
        if let Some(p) = self.hash_progress.as_ref() {
            if let Ok(mut g) = p.lock() {
                g.cancelled = true;
            }
        }
        self.hash_progress = None;
        self.hash_receiver = None;
        self.hash_rate_tracker.reset();
    }

    /// Poll the hashing worker and refresh redump_matches when it finishes.
    fn poll_hash(&mut self) {
        if let Some(progress) = self.hash_progress.as_ref() {
            // Pump the rate tracker each frame while hashing is in flight.
            if let Ok(p) = progress.lock() {
                self.hash_rate_tracker.record(p.current_bytes, &p.stage);
            }
        }
        let Some(rx) = self.hash_receiver.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(hashes)) => {
                self.log(
                    LogLevel::Info,
                    format!(
                        "Hashed {} ({}): sha1={}, md5={}, crc32={}",
                        hashes.source,
                        super::progress::format_size(hashes.size_bytes),
                        &hashes.sha1[..16],
                        &hashes.md5[..16],
                        hashes.crc32,
                    ),
                );
                self.apply_hash_match(&hashes);
                self.hash_progress = None;
                self.hash_receiver = None;
            }
            Ok(Err(e)) => {
                // "Unsupported format" / "cancelled" are not real failures.
                if !e.contains("cancelled") && !e.contains("not yet supported") {
                    log::warn!("Hashing failed: {e}");
                    self.log(LogLevel::Warning, format!("Hashing failed: {e}"));
                } else {
                    log::info!("Hashing skipped: {e}");
                }
                self.hash_progress = None;
                self.hash_receiver = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.hash_progress = None;
                self.hash_receiver = None;
            }
        }
    }

    /// Re-run the redump cascade with hash inputs filled in. Replaces the
    /// existing match list when the hash tier produces a hit, otherwise
    /// leaves the serial/PVD result in place.
    fn apply_hash_match(&mut self, hashes: &crate::disc::hasher::TrackHashes) {
        // Run the lookup first; only touch self.disc_info to apply the
        // result. Keeps the &mut borrow narrow so we can also call self.log.
        let mgr = match crate::db::DatabaseManager::new() {
            Ok(m) => m,
            Err(e) => {
                log::debug!("hash match skipped (manager init): {e}");
                return;
            }
        };
        let conn = match mgr.open() {
            Ok(c) => c,
            Err(e) => {
                log::debug!("hash match skipped: {e}");
                return;
            }
        };
        let inputs = crate::db::CascadeInputs {
            track_sha1: Some(&hashes.sha1),
            track_md5: Some(&hashes.md5),
            track_crc32: Some(&hashes.crc32),
            ..Default::default()
        };
        let matches = match crate::db::lookup::cascade(&conn, &inputs) {
            Ok(Some(m)) if !m.is_empty() => m,
            Ok(_) => {
                self.log(LogLevel::Info, "Redump: no hash match (kept prior match)");
                return;
            }
            Err(e) => {
                log::warn!("hash cascade failed: {e}");
                return;
            }
        };

        let head_summary = {
            let head = &matches[0];
            format!(
                "Redump hash match: {} [#{}] via {:?}",
                head.title, head.redump_id, head.matched_via
            )
        };
        if let Some(Ok(info)) = self.disc_info.as_mut() {
            info.redump_matches = Some(matches);
        }
        self.log(LogLevel::Success, head_summary);
    }

    /// Poll the background DB update channel and log the outcome once.
    fn poll_db_update(&mut self) {
        let Some(rx) = self.db_update_receiver.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(outcome)) => {
                match &outcome {
                    crate::db::UpdateOutcome::UpToDate { .. } => {
                        self.log(LogLevel::Info, "Redump DB is up to date");
                    }
                    crate::db::UpdateOutcome::Updated { local_path, .. } => {
                        self.log(
                            LogLevel::Success,
                            format!("Redump DB updated: {}", local_path.display()),
                        );
                    }
                    crate::db::UpdateOutcome::OfflineUsingCached { error, .. } => {
                        self.log(
                            LogLevel::Warning,
                            format!("Redump DB update skipped (offline): {error}"),
                        );
                    }
                    crate::db::UpdateOutcome::OfflineNoCache { error } => {
                        self.log(
                            LogLevel::Warning,
                            format!("Redump DB unavailable (offline, no cache): {error}"),
                        );
                    }
                }
                self.db_update_done = true;
                self.db_update_receiver = None;
            }
            Ok(Err(e)) => {
                self.log(LogLevel::Error, format!("Redump DB update failed: {e}"));
                self.db_update_done = true;
                self.db_update_receiver = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.db_update_done = true;
                self.db_update_receiver = None;
            }
        }
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

        // Clear browse view state
        self.browse_view.clear();
        self.show_browse_window = false;

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
                let mut info = info;
                self.enrich_with_redump(&mut info);
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
                        redump_matches: None,
                    };

                    let mut fallback_info = fallback_info;
                    self.enrich_with_redump(&mut fallback_info);
                    self.disc_info = Some(Ok(fallback_info));
                } else {
                    self.log(LogLevel::Error, format!("Error reading disc: {}", e));
                    self.disc_info = Some(Err(error_str));
                }
            }
        }
        
        // Clear the log callback after processing
        crate::disc::clear_log_callback();

        // Kick off track hashing in the background. The disc info is already
        // displayed; when hashing finishes we'll re-run the cascade with the
        // hash tier active and refresh `redump_matches`.
        self.start_hashing();
    }

    /// Open file picker dialog
    fn open_file_picker(&mut self) {
        let extensions = supported_extensions();

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

    /// Save search configuration to the per-user `config.json`.
    fn save_search_config(&self) {
        let Ok(path) = crate::config::config_file_path() else { return };
        let mut json: serde_json::Value = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({})),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(_) => return,
        };

        let search = json
            .as_object_mut()
            .expect("config root is an object")
            .entry("search".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(obj) = search.as_object_mut() {
            obj.insert(
                "content_type".to_string(),
                serde_json::Value::String(self.search_config.content_type.as_str().to_string()),
            );
        }

        if let Ok(updated) = serde_json::to_string_pretty(&json) {
            let _ = std::fs::write(&path, updated);
        }
    }

    /// Start an async image search
    fn start_search(&mut self, query: &str) {
        let query = query.to_string();
        let user_agent = self.search_config.user_agent.clone();
        let (tx, rx) = mpsc::channel();

        self.search_in_progress = true;
        self.search_results.clear();
        self.selected_image_index = None;
        self.search_receiver = Some(rx);
        self.show_search_window = true;

        thread::spawn(move || {
            let result = crate::search::search_images_with_ua(&query, 20, user_agent.as_deref());
            let _ = tx.send(result);
        });
    }

    /// Start async user agent capture from browser
    fn start_user_agent_capture(&mut self) {
        let (tx, rx) = mpsc::channel();

        self.user_agent_capture_in_progress = true;
        self.user_agent_receiver = Some(rx);

        thread::spawn(move || {
            let result = crate::search::capture_browser_user_agent();
            let _ = tx.send(result);
        });
    }

    /// Start an async MusicBrainz search using disc ID
    /// If fallback_query is provided and MusicBrainz returns no results, fall back to DDG search
    fn start_musicbrainz_search(&mut self, disc_id: &str, toc_string: Option<String>, fallback_query: Option<String>) {
        let disc_id = disc_id.to_string();
        let user_agent = self.search_config.user_agent.clone();
        let (tx, rx) = mpsc::channel();

        self.search_in_progress = true;
        self.search_results.clear();
        self.selected_image_index = None;
        self.search_receiver = Some(rx);
        self.show_search_window = true;

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

                // If we got at least one MusicBrainz result, search Discogs API for the album
                if let Some(first_release) = releases.first() {
                    log::info!("Searching Discogs API for album: {} - {}", first_release.artist, first_release.title);

                    // Use Discogs API directly instead of DDG
                    match crate::api::discogs_search(&first_release.artist, &first_release.title) {
                        Ok(discogs_results) => {
                            // Convert Discogs results to ImageResult format
                            let discogs_images: Vec<_> = discogs_results.iter()
                                .filter_map(|result| {
                                    // Need at least a thumbnail or cover image
                                    let image_url = result.image_url.clone()
                                        .or_else(|| result.thumbnail_url.clone())?;
                                    let thumbnail = result.thumbnail_url.clone()
                                        .unwrap_or_else(|| image_url.clone());

                                    let title = if let Some(year) = result.year {
                                        format!("{} - {} ({})", result.artist, result.title, year)
                                    } else {
                                        format!("{} - {}", result.artist, result.title)
                                    };

                                    Some(crate::search::ImageResult {
                                        image_url,
                                        thumbnail_url: thumbnail,
                                        title,
                                        source: format!("Discogs ({})", result.discogs_id),
                                        width: None,
                                        height: None,
                                    })
                                })
                                .collect();

                            log::info!("Discogs API returned {} results with images", discogs_images.len());
                            all_results.extend(discogs_images);
                        }
                        Err(e) => {
                            log::warn!("Failed to search Discogs API: {}", e);
                        }
                    }
                }

                Ok(all_results)
            });

            // If MusicBrainz returned no results and we have a fallback query, use DDG
            let final_result = match result {
                Ok(results) if results.is_empty() => {
                    if let Some(query) = fallback_query {
                        log::info!("MusicBrainz returned no results, falling back to DDG search");
                        crate::search::search_images_with_ua(&query, 20, user_agent.as_deref())
                    } else {
                        Ok(results)
                    }
                }
                Err(e) => {
                    // MusicBrainz failed, try fallback
                    if let Some(query) = fallback_query {
                        log::warn!("MusicBrainz search failed: {}, falling back to DDG search", e);
                        crate::search::search_images_with_ua(&query, 20, user_agent.as_deref())
                    } else {
                        Err(e)
                    }
                }
                other => other,
            };

            let _ = tx.send(final_result);
        });
    }

    /// Poll for search results
    fn poll_search(&mut self) {
        if let Some(ref receiver) = self.search_receiver {
            match receiver.try_recv() {
                Ok(Ok(mut results)) => {
                    let count = results.len();

                    // Always clear previous results and selection
                    self.search_results.clear();
                    self.selected_image_index = None;

                    if count == 0 {
                        // No results found
                        self.search_in_progress = false;
                        self.search_receiver = None;
                        self.log(LogLevel::Warning, "No results found");
                        return;
                    }

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
                    self.search_results.clear();
                    self.selected_image_index = None;
                    self.search_in_progress = false;
                    self.search_receiver = None;
                    self.log(LogLevel::Error, format!("Search failed: {}", e));
                }
                Err(TryRecvError::Empty) => {
                    // Still searching, keep waiting
                }
                Err(TryRecvError::Disconnected) => {
                    self.search_results.clear();
                    self.selected_image_index = None;
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

    /// Poll for global log messages (from the UiLogger)
    fn poll_global_logs(&mut self) {
        if let Some(ref receiver) = self.global_log_receiver {
            // Drain all available log messages
            while let Ok(msg) = receiver.try_recv() {
                // Parse log level from the message format: "[LEVEL] target: message"
                let level = if msg.starts_with("[ERROR]") {
                    LogLevel::Error
                } else if msg.starts_with("[WARN]") {
                    LogLevel::Warning
                } else if msg.starts_with("[INFO]") {
                    LogLevel::Info
                } else {
                    // DEBUG and TRACE are shown as Info in the UI
                    LogLevel::Info
                };

                self.log_messages.push(LogMessage {
                    text: msg,
                    level,
                });

                // Keep only last 100 messages
                if self.log_messages.len() > 100 {
                    self.log_messages.remove(0);
                }
            }
        }
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

    /// Poll for user agent capture results
    fn poll_user_agent_capture(&mut self) {
        if let Some(ref receiver) = self.user_agent_receiver {
            match receiver.try_recv() {
                Ok(Ok(user_agent)) => {
                    self.user_agent_capture_in_progress = false;
                    self.user_agent_receiver = None;

                    // Save to config file
                    if let Err(e) = crate::search::save_user_agent_to_config(&user_agent) {
                        self.log(LogLevel::Error, format!("Failed to save user agent: {}", e));
                    } else {
                        self.log(LogLevel::Info, "Browser identity captured and saved".to_string());
                        // Reload the search config to pick up the new user agent
                        self.search_config = SearchConfig::default();
                    }
                }
                Ok(Err(e)) => {
                    self.user_agent_capture_in_progress = false;
                    self.user_agent_receiver = None;
                    self.log(LogLevel::Error, format!("Failed to capture browser identity: {}", e));
                }
                Err(TryRecvError::Empty) => {
                    // Still waiting for browser
                }
                Err(TryRecvError::Disconnected) => {
                    self.user_agent_capture_in_progress = false;
                    self.user_agent_receiver = None;
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Poll for global log messages
        self.poll_global_logs();

        // Poll for search results
        self.poll_search();

        // Poll for preview image
        self.poll_preview(&ctx);

        // Poll for export results
        self.poll_export();

        // Poll for update check
        self.poll_update_check();

        // Poll for user agent capture
        self.poll_user_agent_capture();

        // Poll for redump DB update result
        self.poll_db_update();

        // Poll the track-hashing worker
        self.poll_hash();

        // Request repaint while loading
        if self.search_in_progress || self.preview_loading || self.export_in_progress || self.user_agent_capture_in_progress || self.hash_progress.is_some() {
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
        egui::Panel::top("top_panel").show_inside(ui, |ui| {
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
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Clear").clicked() {
                            self.log_messages.clear();
                        }
                        if ui.button("Copy").clicked() {
                            let joined = self
                                .log_messages
                                .iter()
                                .map(|m| m.text.as_str())
                                .collect::<Vec<_>>()
                                .join("\n");
                            ui.ctx().copy_text(joined);
                        }
                        if ui.button("Settings").clicked() {
                            self.show_log_settings = true;
                        }
                    });
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

        // Log settings dialog
        if self.show_log_settings {
            let mut open = self.show_log_settings;
            let mut new_level: Option<String> = None;
            egui::Window::new("Log Settings")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(&ctx, |ui| {
                    ui.label("Log level:");
                    egui::ComboBox::from_id_salt("log_level_combo")
                        .selected_text(&self.log_level)
                        .show_ui(ui, |ui| {
                            for level in ["error", "warn", "info", "debug", "trace", "off"] {
                                if ui
                                    .selectable_label(self.log_level == level, level)
                                    .clicked()
                                {
                                    new_level = Some(level.to_string());
                                }
                            }
                        });
                    ui.add_space(4.0);
                    ui.label("Applies immediately and is saved to config.json.");
                });
            self.show_log_settings = open;
            if let Some(level) = new_level {
                self.log_level = level.clone();
                log::set_max_level(crate::logging::ui_logger::parse_level(&level));
                if let Err(e) = crate::config::save_config_field(
                    "log_level",
                    serde_json::Value::String(level),
                ) {
                    self.log(LogLevel::Error, format!("Failed to save log level: {e}"));
                }
            }
        }

        // Browse window (for filesystem browsing)
        if self.show_browse_window {
            let disc_info_clone = self.disc_info.as_ref().and_then(|r| r.as_ref().ok()).cloned();

            // Draw the window at 75% of the app's content area, and fill its
            // interior to 95% of that so it reads as a floating window with a
            // small margin rather than spilling past the app edges.
            let app_rect = ctx.content_rect().size();
            let default_w = (app_rect.x * 0.75).max(600.0);
            let default_h = (app_rect.y * 0.75).max(400.0);

            egui::Window::new("Browse Disc Contents")
                .open(&mut self.show_browse_window)
                .default_size([default_w, default_h])
                .min_width(600.0)
                .min_height(400.0)
                .resizable(true)
                .show(&ctx, |ui| {
                    // Fill the interior to 95% of the window height; the inner
                    // panes use this value directly so they neither collapse nor
                    // overrun the window.
                    let content_h = default_h * 0.95;
                    ui.set_min_height(content_h);
                    if let Some(ref info) = disc_info_clone {
                        self.browse_view.show(ui, info, content_h);
                    } else {
                        ui.label("No disc loaded");
                    }
                });

            // Clear browse view if window was closed
            if !self.show_browse_window {
                self.browse_view.clear();
            }
        }

        // Artwork Search window — opens automatically when a search is started.
        if self.show_search_window {
            // Pre-compute values needed by both the window UI and the post-render
            // handlers, so we don't have to re-borrow `self.disc_info` inside the
            // window closure (which already mutably borrows other fields of self).
            let info_for_search: Option<DiscInfo> = self
                .disc_info
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .cloned();
            let default_query = info_for_search
                .as_ref()
                .map(|info| {
                    ArtworkSearchQuery::from_disc_info_with_config(info, &self.search_config)
                        .build_query()
                })
                .unwrap_or_default();
            let use_musicbrainz = info_for_search
                .as_ref()
                .map(|i| i.toc.is_some())
                .unwrap_or(false);
            let disc_id = info_for_search
                .as_ref()
                .and_then(|i| i.toc.as_ref().map(|toc| toc.musicbrainz_id()));
            let toc_string_for_browser = info_for_search
                .as_ref()
                .and_then(|i| i.toc.as_ref().map(|toc| toc.to_toc_string()));
            let preview_loading = self.preview_loading;
            let search_in_progress = self.search_in_progress;
            let has_disc = info_for_search.is_some();

            let mut content_type_changed = false;
            let mut reset_query_clicked = false;
            let mut search_clicked = false;
            let mut browser_clicked = false;
            let mut manual_preview_clicked = false;
            let mut start_export_data: Option<(String, String)> = None;
            let mut selected_idx_change: Option<usize> = None;

            // Draw the window at 75% width / 85% height of the app's content
            // area — sized so 20 results fill it without much dead space.
            let app_rect = ctx.content_rect().size();
            let default_w = (app_rect.x * 0.75).max(600.0);
            let default_h = (app_rect.y * 0.85).max(500.0);

            egui::Window::new("Artwork Search")
                .open(&mut self.show_search_window)
                .default_size([default_w, default_h])
                .min_width(600.0)
                .min_height(400.0)
                .resizable(true)
                .show(&ctx, |ui| {
                    // ---- Content type ----
                    ui.horizontal(|ui| {
                        ui.label("Content Type:");
                        egui::ComboBox::new("search_window_content_type", "")
                            .selected_text(self.search_config.content_type.display_name())
                            .show_ui(ui, |ui| {
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::Any, "Any").clicked() { content_type_changed = true; }
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::Games, "Games").clicked() { content_type_changed = true; }
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::AppsUtilities, "Apps & Utilities").clicked() { content_type_changed = true; }
                                if ui.selectable_value(&mut self.search_config.content_type, ContentType::AudioCDs, "Audio CDs").clicked() { content_type_changed = true; }
                            });
                    });

                    ui.add_space(6.0);

                    // ---- Search query (textbox + Reset on its own row so
                    //      the trigger buttons below don't fight for width) ----
                    ui.horizontal(|ui| {
                        ui.label("Search:");
                        let avail = ui.available_width() - 80.0; // Reset button + spacing
                        ui.add(
                            egui::TextEdit::singleline(&mut self.search_query_text)
                                .desired_width(avail.max(200.0))
                                .hint_text("Refine search query..."),
                        );
                        if ui.button("Reset").clicked() {
                            reset_query_clicked = true;
                        }
                    });

                    ui.add_space(4.0);

                    // ---- Trigger buttons on their own row ----
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(!search_in_progress && has_disc, |ui| {
                            let label = if use_musicbrainz { "Search MusicBrainz" } else { "Search" };
                            if ui.button(label).clicked() { search_clicked = true; }
                            if ui.button("Open in Browser").clicked() { browser_clicked = true; }
                        });
                        if search_in_progress {
                            ui.spinner();
                            ui.label("Searching...");
                        }
                    });

                    ui.add_space(6.0);

                    // ---- Manual URL ----
                    ui.horizontal(|ui| {
                        ui.label("Manual URL:");
                        let avail = ui.available_width() - 95.0; // Preview button + spacing
                        ui.add(
                            egui::TextEdit::singleline(&mut self.manual_url)
                                .desired_width(avail.max(160.0))
                                .hint_text("Paste image URL here..."),
                        );
                        let can_preview_manual = !self.manual_url.is_empty() && !preview_loading;
                        if ui.add_enabled(can_preview_manual, egui::Button::new("Preview")).clicked() {
                            manual_preview_clicked = true;
                        }
                    });

                    ui.separator();

                    // ---- Results + preview pane ----
                    ui.horizontal_top(|ui| {
                        // Left: results list
                        ui.vertical(|ui| {
                            ui.heading("Search Results");
                            if self.search_results.is_empty() {
                                let msg = if search_in_progress { "Searching..." } else { "No results yet." };
                                ui.colored_label(egui::Color32::GRAY, msg);
                            } else {
                                ui.label(format!(
                                    "{} images - click to preview",
                                    self.search_results.len()
                                ));
                            }
                            ui.add_space(4.0);

                            let results_height = ui.available_height().max(150.0);
                            egui::ScrollArea::vertical()
                                .id_salt("search_window_results")
                                .auto_shrink([false, false])
                                .max_height(results_height)
                                .max_width(360.0)
                                .show(ui, |ui| {
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
                                            selected_idx_change = Some(idx);
                                        }
                                        let response = response.on_hover_text(&result.image_url);
                                        response.context_menu(|ui| {
                                            if ui.button("Copy URL to clipboard").clicked() {
                                                ui.ctx().copy_text(result.image_url.clone());
                                                ui.close();
                                            }
                                            if ui.button("Open in browser").clicked() {
                                                let _ = open_in_browser(&result.image_url);
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                        });

                        ui.separator();

                        // Right: preview
                        ui.vertical(|ui| {
                            ui.heading("Preview");

                            if self.preview_loading {
                                ui.add_space(20.0);
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("Loading...");
                                });
                            } else if let Some(ref texture) = self.preview_texture {
                                let size = texture.size_vec2();
                                let max_size = 280.0;
                                let scale = (max_size / size.x).min(max_size / size.y).min(1.0);
                                let display_size = egui::vec2(size.x * scale, size.y * scale);

                                let texture_id = texture.id();
                                let img_width = size.x as u32;
                                let img_height = size.y as u32;
                                let can_download = self.selected_path.is_some()
                                    && self.preview_url.is_some()
                                    && !self.export_in_progress;
                                let export_in_progress = self.export_in_progress;
                                let output_path = self
                                    .selected_path
                                    .as_ref()
                                    .map(|p| generate_output_path(p));
                                let preview_url = self.preview_url.clone();

                                ui.image((texture_id, display_size));
                                ui.label(format!("{}x{}", img_width, img_height));
                                ui.add_space(8.0);

                                ui.add_enabled_ui(can_download, |ui| {
                                    let btn_text = if export_in_progress {
                                        "Downloading..."
                                    } else {
                                        "Download & Save"
                                    };
                                    if ui.button(btn_text).clicked() {
                                        if let (Some(url), Some(path)) =
                                            (preview_url.clone(), output_path.clone())
                                        {
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
                                if let Some(ref path) = output_path {
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new("Save to:")
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                    ui.label(
                                        egui::RichText::new(path)
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                }
                            } else if let Some(ref error) = self.preview_error {
                                ui.add_space(20.0);
                                ui.colored_label(egui::Color32::RED, error);
                                ui.add_space(10.0);
                                ui.label("Tip: Download the image manually and drop it here");
                                let output_path = self
                                    .selected_path
                                    .as_ref()
                                    .map(|p| generate_output_path(p));
                                if let Some(ref path) = output_path {
                                    ui.add_space(10.0);
                                    ui.label(
                                        egui::RichText::new(format!("Will save to: {}", path))
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                }
                            } else {
                                ui.add_space(20.0);
                                ui.label("Select an image to preview");
                            }
                        });
                    });
                });

            // ---- Handlers for the search window ----
            if content_type_changed {
                self.save_search_config();
                self.update_search_query_from_disc();
            }
            if reset_query_clicked {
                self.search_query_text = default_query;
            }
            if let Some(idx) = selected_idx_change {
                self.selected_image_index = Some(idx);
                let url = self.search_results.get(idx).map(|r| r.image_url.clone());
                if let Some(url) = url {
                    self.load_preview(&url);
                }
            }
            if search_clicked {
                let query_for_search = self.search_query_text.clone();
                if use_musicbrainz {
                    if let Some(ref id) = disc_id {
                        self.log(
                            LogLevel::Info,
                            format!("Searching MusicBrainz for disc ID: {}", id),
                        );
                        let toc = info_for_search
                            .as_ref()
                            .and_then(|i| i.toc.as_ref())
                            .map(|toc| toc.to_toc_string());
                        self.start_musicbrainz_search(id, toc, Some(query_for_search.clone()));
                    }
                } else {
                    self.log(LogLevel::Info, format!("Searching: {}", query_for_search));
                    self.start_search(&query_for_search);
                }
            }
            if browser_clicked {
                let query_for_search = self.search_query_text.clone();
                if use_musicbrainz {
                    if let Some(ref id) = disc_id {
                        let mut url = format!(
                            "https://musicbrainz.org/ws/2/discid/{}?fmt=json&inc=artist-credits+release-groups",
                            id
                        );
                        if let Some(ref toc_str) = toc_string_for_browser {
                            url.push_str(&format!("&toc={}", toc_str));
                        }
                        self.log(LogLevel::Info, format!("Opening MusicBrainz: {}", url));
                        if let Err(e) = open_in_browser(&url) {
                            self.log(LogLevel::Error, e);
                        }
                    }
                } else {
                    let encoded = urlencoding::encode(&query_for_search);
                    let browser_url = format!(
                        "https://www.google.com/search?tbm=isch&tbs=iar:s&q={}",
                        encoded
                    );
                    self.log(
                        LogLevel::Info,
                        format!("Opening browser: {}", query_for_search),
                    );
                    if let Err(e) = open_in_browser(&browser_url) {
                        self.log(LogLevel::Error, e);
                    }
                }
            }
            if manual_preview_clicked {
                let url = self.manual_url.clone();
                if !url.is_empty() {
                    self.load_preview(&url);
                }
            }
            if let Some((url, path)) = start_export_data {
                self.start_export(&url, &path);
            }
        }

        // Main central panel
        egui::CentralPanel::default().show_inside(ui, |ui| {
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

                    ui.add_space(8.0);

                    // Browser Identity section
                    ui.horizontal(|ui| {
                        ui.label("Browser Identity:");
                        if self.search_config.user_agent.is_some() {
                            ui.label(egui::RichText::new("Configured").color(egui::Color32::GREEN));
                        } else {
                            ui.label(egui::RichText::new("Not set").color(egui::Color32::YELLOW));
                        }
                    });

                    ui.horizontal(|ui| {
                        let button_enabled = !self.user_agent_capture_in_progress;
                        let button_text = if self.user_agent_capture_in_progress {
                            "Waiting for browser..."
                        } else {
                            "Configure Browser Identity"
                        };

                        if ui.add_enabled(button_enabled, egui::Button::new(button_text)).clicked() {
                            self.start_user_agent_capture();
                        }
                    });
                });
            });

            ui.add_space(16.0);

            // Disc information section
            ui.group(|ui| {
                ui.heading("Disc Information");
                ui.add_space(8.0);

                // Snapshot the hashing state outside the match so we can reach
                // both self.hash_progress (worker state) and self.hash_rate_tracker
                // (UI-thread state) without fighting the borrow checker inside
                // the egui closures below.
                let hash_snapshot: Option<HashRowSnapshot> = self
                    .hash_progress
                    .as_ref()
                    .and_then(|p| p.lock().ok().map(|g| (g.current_bytes, g.total_bytes, g.stage.clone(), g.active)))
                    .and_then(|(cur, tot, stage, active)| {
                        if !active || tot == 0 {
                            None
                        } else {
                            Some(HashRowSnapshot {
                                fraction: (cur as f32 / tot as f32).clamp(0.0, 1.0),
                                current_bytes: cur,
                                total_bytes: tot,
                                stage,
                                rate_suffix: self.hash_rate_tracker.suffix(cur, tot),
                            })
                        }
                    });
                let hashing_in_progress = hash_snapshot.is_some();

                match &self.disc_info {
                    Some(Ok(info)) => {
                        // Pre-compute scalar/cloned values used by both columns and
                        // the click handlers below so we don't have to borrow `info`
                        // (or `self`) inside the column closure.
                        let search_query = ArtworkSearchQuery::from_disc_info_with_config(info, &self.search_config);
                        let default_query = search_query.build_query();
                        if self.search_query_text.is_empty() {
                            self.search_query_text = default_query.clone();
                        }
                        let can_browse = info.filesystem != FilesystemType::Unknown;
                        let use_musicbrainz = info.toc.is_some();
                        let disc_id = info.toc.as_ref().map(|toc| toc.musicbrainz_id());
                        let toc_string_for_browser = info.toc.as_ref().map(|toc| toc.to_toc_string());
                        let preview_loading = self.preview_loading;
                        let search_in_progress = self.search_in_progress;

                        let mut browse_clicked = false;
                        let mut search_clicked = false;
                        let mut browser_clicked = false;
                        let mut manual_preview_clicked = false;
                        let mut reset_query_clicked = false;

                        // Stretch the value column so the table fills most of the
                        // panel width instead of shrinking to its content.
                        let value_col_w = (ui.available_width() - 200.0).max(320.0);
                        egui::Grid::new("disc_info_grid")
                            .num_columns(2)
                            .spacing([40.0, 4.0])
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("Volume Label:");
                                ui.horizontal(|ui| {
                                    ui.set_min_width(value_col_w);
                                    if let Some(ref label) = info.volume_label {
                                        ui.strong(label);
                                    } else {
                                        ui.colored_label(egui::Color32::GRAY, "Not found");
                                    }
                                });
                                ui.end_row();

                                ui.label("Format:");
                                ui.label(info.format.display_name());
                                ui.end_row();

                                ui.label("Filesystem:");
                                ui.horizontal(|ui| {
                                    ui.label(info.filesystem.display_name());
                                    if can_browse {
                                        if ui.button("Browse Contents...").clicked() {
                                            browse_clicked = true;
                                        }
                                    } else {
                                        ui.colored_label(
                                            egui::Color32::GRAY,
                                            "(no browser)",
                                        );
                                    }
                                });
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

                                // Hashing progress row (only while a worker is active)
                                if let Some(ref h) = hash_snapshot {
                                    ui.label("Hashing:");
                                    let text = format!(
                                        "{} / {} ({:.0}%){}",
                                        super::progress::format_size(h.current_bytes),
                                        super::progress::format_size(h.total_bytes),
                                        h.fraction * 100.0,
                                        h.rate_suffix,
                                    );
                                    ui.add(
                                        egui::ProgressBar::new(h.fraction)
                                            .text(text)
                                            .animate(true),
                                    );
                                    ui.end_row();
                                }

                                // Redump database match
                                if let Some(matches) = &info.redump_matches {
                                    ui.label("Redump:");
                                    if let Some(first) = matches.first() {
                                        let (color, label) = match_styling(first.matched_via);
                                        ui.vertical(|ui| {
                                            // Header line: match badge + jump link.
                                            ui.horizontal(|ui| {
                                                ui.colored_label(
                                                    color,
                                                    format!(
                                                        "{label}: {} (#{})",
                                                        first.title, first.redump_id
                                                    ),
                                                );
                                                ui.separator();
                                                ui.hyperlink_to(
                                                    "View on redump.org",
                                                    &first.redump_url,
                                                );
                                            });
                                            if let Some(ft) = first.foreign_title.as_ref().filter(|s| !s.is_empty()) {
                                                ui.label(
                                                    egui::RichText::new(ft)
                                                        .small()
                                                        .italics()
                                                        .color(egui::Color32::LIGHT_GRAY),
                                                );
                                            }

                                            // Structured fields, laid out as two
                                            // key/value pairs per row so they spread
                                            // horizontally instead of a tall column.
                                            let present: Vec<(&str, &str)> = [
                                                ("System", Some(first.system.as_str())),
                                                ("Media", first.media.as_deref()),
                                                ("Category", first.category.as_deref()),
                                                ("Edition", first.edition.as_deref()),
                                                ("Version", first.version.as_deref()),
                                            ]
                                            .into_iter()
                                            .filter_map(|(k, v)| {
                                                v.filter(|s| !s.is_empty()).map(|val| (k, val))
                                            })
                                            .collect();

                                            if !present.is_empty() {
                                                ui.add_space(6.0);
                                                let cell = |ui: &mut egui::Ui, k: &str, v: &str| {
                                                    ui.label(
                                                        egui::RichText::new(format!("{k}:"))
                                                            .color(egui::Color32::GRAY),
                                                    );
                                                    ui.label(v);
                                                };
                                                egui::Grid::new(format!("redump_sub_{}", first.redump_id))
                                                    .num_columns(4)
                                                    .spacing([24.0, 6.0])
                                                    .show(ui, |ui| {
                                                        for pair in present.chunks(2) {
                                                            cell(ui, pair[0].0, pair[0].1);
                                                            if let Some(second) = pair.get(1) {
                                                                cell(ui, second.0, second.1);
                                                            } else {
                                                                ui.label("");
                                                                ui.label("");
                                                            }
                                                            ui.end_row();
                                                        }
                                                    });
                                            }

                                            if matches.len() > 1 {
                                                ui.collapsing(
                                                    format!(
                                                        "{} other candidate(s)",
                                                        matches.len() - 1
                                                    ),
                                                    |ui| {
                                                        for m in matches.iter().skip(1) {
                                                            ui.horizontal(|ui| {
                                                                ui.label(format!(
                                                                    "#{}: {}",
                                                                    m.redump_id, m.title
                                                                ));
                                                                ui.hyperlink_to(
                                                                    "↗",
                                                                    &m.redump_url,
                                                                );
                                                            });
                                                        }
                                                    },
                                                );
                                            }
                                        });
                                    } else if hashing_in_progress {
                                        ui.colored_label(
                                            egui::Color32::LIGHT_GRAY,
                                            "Searching… (waiting on hash)",
                                        );
                                    } else {
                                        ui.colored_label(egui::Color32::GRAY, "No match");
                                    }
                                    ui.end_row();
                                } else if hashing_in_progress {
                                    ui.label("Redump:");
                                    ui.colored_label(
                                        egui::Color32::LIGHT_GRAY,
                                        "Searching… (waiting on hash)",
                                    );
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
                                    let disc_id = toc.musicbrainz_id();
                                    let toc_string = toc.to_toc_string();
                                    ui.horizontal(|ui| {
                                        ui.label(&disc_id);
                                        if ui.small_button("📋").on_hover_text("Copy to clipboard").clicked() {
                                            ui.ctx().copy_text(disc_id.clone());
                                        }
                                        if ui.small_button("🔍").on_hover_text("Search on MusicBrainz").clicked() {
                                            // Use the exact same URL as the API lookup
                                            let url = format!(
                                                "https://musicbrainz.org/ws/2/discid/{}?fmt=json&inc=artist-credits+release-groups&toc={}",
                                                disc_id, toc_string
                                            );
                                            let _ = crate::api::open_in_browser(&url);
                                        }
                                    });
                                    ui.end_row();
                                }

                                // HFS information
                                if let Some(ref mdb) = info.hfs_mdb {
                                    ui.label("Files:");
                                    ui.label(format!("{}", mdb.file_count));
                                    ui.end_row();
                                }

                                if let Some(ref header) = info.hfsplus_header {
                                    ui.label("HFS+ Version:");
                                    ui.label(format!("{}", header.version));
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

                        ui.add_space(12.0);

                        // ---- Search controls (full width) — triggers the Artwork Search window
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            let avail = ui.available_width() - 70.0;
                            ui.add(
                                egui::TextEdit::singleline(&mut self.search_query_text)
                                    .desired_width(avail.max(200.0))
                                    .hint_text("Enter search query..."),
                            );
                            if ui.button("Reset").clicked() {
                                reset_query_clicked = true;
                            }
                        });

                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!search_in_progress, |ui| {
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
                            if search_in_progress {
                                ui.spinner();
                                ui.label("Searching...");
                            }
                        });

                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            ui.label("Manual URL:");
                            let avail = ui.available_width() - 90.0;
                            ui.add(
                                egui::TextEdit::singleline(&mut self.manual_url)
                                    .desired_width(avail.max(200.0))
                                    .hint_text("Paste image URL here..."),
                            );
                            let can_preview_manual =
                                !self.manual_url.is_empty() && !preview_loading;
                            if ui
                                .add_enabled(can_preview_manual, egui::Button::new("Preview"))
                                .clicked()
                            {
                                manual_preview_clicked = true;
                            }
                        });

                        // ---- Click handlers (outside ui.columns so &mut self calls
                        // don't fight with the column closure's field borrows) ----
                        if reset_query_clicked {
                            self.search_query_text = default_query.clone();
                        }

                        if browse_clicked {
                            let info_clone = info.clone();
                            self.show_browse_window = true;
                            if !self.browse_view.is_active() {
                                match self.browse_view.initialize(&info_clone) {
                                    Ok(()) => {
                                        self.log_messages.push(LogMessage {
                                            text: "Opened filesystem for browsing".to_string(),
                                            level: LogLevel::Success,
                                        });
                                    }
                                    Err(e) => {
                                        self.log_messages.push(LogMessage {
                                            text: format!("Failed to open filesystem: {}", e),
                                            level: LogLevel::Error,
                                        });
                                        self.show_browse_window = false;
                                    }
                                }
                            }
                        }

                        if search_clicked {
                            let query_for_search = self.search_query_text.clone();
                            if use_musicbrainz {
                                if let Some(ref id) = disc_id {
                                    self.log(
                                        LogLevel::Info,
                                        format!("Searching MusicBrainz for disc ID: {}", id),
                                    );
                                    let toc = self
                                        .disc_info
                                        .as_ref()
                                        .and_then(|result| result.as_ref().ok())
                                        .and_then(|info| info.toc.as_ref())
                                        .map(|toc| toc.to_toc_string());
                                    self.start_musicbrainz_search(
                                        id,
                                        toc,
                                        Some(query_for_search.clone()),
                                    );
                                } else {
                                    self.log(LogLevel::Error, "No disc ID available");
                                }
                            } else {
                                self.log(
                                    LogLevel::Info,
                                    format!("Searching: {}", query_for_search),
                                );
                                self.start_search(&query_for_search);
                            }
                        }

                        if browser_clicked {
                            let query_for_search = self.search_query_text.clone();
                            if use_musicbrainz {
                                if let Some(ref id) = disc_id {
                                    let mut url = format!(
                                        "https://musicbrainz.org/ws/2/discid/{}?fmt=json&inc=artist-credits+release-groups",
                                        id
                                    );
                                    if let Some(ref toc_str) = toc_string_for_browser {
                                        url.push_str(&format!("&toc={}", toc_str));
                                    }
                                    self.log(
                                        LogLevel::Info,
                                        format!("Opening MusicBrainz: {}", url),
                                    );
                                    if let Err(e) = open_in_browser(&url) {
                                        self.log(LogLevel::Error, e);
                                    }
                                }
                            } else {
                                let encoded = urlencoding::encode(&query_for_search);
                                let browser_url = format!(
                                    "https://www.google.com/search?tbm=isch&tbs=iar:s&q={}",
                                    encoded
                                );
                                self.log(
                                    LogLevel::Info,
                                    format!("Opening browser: {}", query_for_search),
                                );
                                if let Err(e) = open_in_browser(&browser_url) {
                                    self.log(LogLevel::Error, e);
                                }
                            }
                        }

                        if manual_preview_clicked {
                            let url = self.manual_url.clone();
                            if !url.is_empty() {
                                self.load_preview(&url);
                            }
                        }

                        if !self.search_results.is_empty() {
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

        });

        // Show drag-and-drop preview
        preview_files_being_dropped(&ctx);

        // Show update notification dialog
        if self.show_update_notification {
            if let Some(update_info) = self.update_info.clone() {
                egui::Window::new("Update Available")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(&ctx, |ui| {
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

        let screen_rect = ctx.content_rect();
        painter.rect_filled(screen_rect, 0.0, Color32::from_black_alpha(192));
        painter.text(
            screen_rect.center(),
            Align2::CENTER_CENTER,
            "Drop disc image to scan",
            TextStyle::Heading.resolve(&ctx.global_style()),
            Color32::WHITE,
        );
    }
}

/// Captured for one frame so the disc-info grid can render the hashing row
/// without re-locking `hash_progress` mid-closure.
struct HashRowSnapshot {
    fraction: f32,
    current_bytes: u64,
    total_bytes: u64,
    #[allow(dead_code)]
    stage: String,
    rate_suffix: String,
}

/// Map a redump match source to (badge color, human label).
fn match_styling(source: crate::db::MatchSource) -> (egui::Color32, &'static str) {
    use crate::db::MatchSource as M;
    match source {
        M::TrackSha1 | M::TrackMd5 | M::TrackCrc32 => {
            (egui::Color32::GREEN, "Confirmed (hash)")
        }
        M::Serial => (egui::Color32::GREEN, "Confirmed (serial)"),
        M::Barcode => (egui::Color32::GREEN, "Confirmed (barcode)"),
        M::PvdVolumeId => (egui::Color32::YELLOW, "Likely (volume label)"),
        M::FuzzyTitle => (egui::Color32::from_rgb(220, 200, 80), "Possible (title)"),
    }
}
