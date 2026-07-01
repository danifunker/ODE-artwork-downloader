//! Main application state and UI implementation

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::api::{open_in_browser, ArtworkSearchQuery, SearchConfig, ContentType};
use crate::disc::{supported_extensions, parse_filename, ConfidenceLevel, DiscInfo, DiscReader, DiscFormat, FilesystemType};
use crate::export::{
    export_artwork, export_artwork_from_url_with_disc, export_artwork_from_url_with_label,
    generate_output_path, ExportResult, ExportSettings,
};
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
    /// Active bulk-processing queue, or `None` when not in bulk mode.
    bulk_queue: Option<super::bulk::BulkQueue>,
    /// In-flight loader dialog state for "Open Bulk Job…".
    bulk_loader: Option<BulkLoaderDialog>,
    /// Cursor index of the last bulk item we loaded into the central panel.
    /// Used by tick_bulk to detect "queue advanced, load next" transitions.
    bulk_loaded_cursor: Option<usize>,
    /// While true, `process_file` will skip the normal redump cascade/fuzzy
    /// auto-runs. Set by tick_bulk before triggering a bulk-driven load.
    bulk_suppress_cascade: bool,
    /// Hash cascade result for the current bulk item — written to the done
    /// log when the user resolves the item. None when hashing hasn't finished
    /// or no hash hit was returned.
    pending_hash_redump_id: Option<i64>,
    /// URL of the artwork the user is currently saving. Read by poll_export
    /// on success so a bulk-mode save can record the chosen URL.
    pending_export_url: Option<String>,
    /// Bottom Y coordinate of the most recently rendered bulk banner.
    /// Used to pick the default position of the Artwork Search window
    /// so it opens directly below the controls in bulk mode.
    bulk_banner_bottom_y: Option<f32>,
    /// Active "broken cue — delete?" prompt. Blocks the loading flow until
    /// the user clicks Delete / Keep, or (in bulk mode) the auto-skip
    /// timeout elapses.
    broken_cue_prompt: Option<BrokenCuePrompt>,
    /// CD audio tracks for the loaded disc, when it's a CHD with audio. `None`
    /// for non-CHD discs or CHDs without parseable tracks. Cached at load time
    /// so the panel doesn't re-open the CHD every frame.
    audio_tracks: Option<Vec<crate::disc::chd_audio::ChdCdTrack>>,
    /// Active CD-DA playback job, if the user is playing a track.
    audio_playback: Option<super::audio::AudioPlayback>,
}

/// Pending decision for a cue file whose referenced BIN(s) don't exist.
struct BrokenCuePrompt {
    cue_path: PathBuf,
    missing: Vec<String>,
    total_refs: usize,
    /// True when the cue came up through bulk-mode processing. Drives the
    /// auto-skip countdown and the queue-advance side-effect on Keep.
    in_bulk: bool,
    /// Wall-clock when the prompt was opened. The bulk countdown reads this
    /// and dismisses with Keep after `BULK_AUTO_SKIP_SECS`.
    opened_at: std::time::Instant,
}

const BULK_AUTO_SKIP_SECS: u64 = 10;

/// Modal state for the bulk-job loader: pending file the user picked, plus
/// the load-time toggles (reprocess existing art, include fuzzy with a
/// minimum confidence) and any parse error to surface.
struct BulkLoaderDialog {
    path: PathBuf,
    item_count: usize,
    /// Items already present in the sidecar `.done.jsonl` log.
    resumed_count: usize,
    reprocess_existing: bool,
    filter: super::bulk::QueueFilter,
    /// Last filter we re-parsed under, so we don't re-read the file on every
    /// frame the dialog is open.
    last_filter: super::bulk::QueueFilter,
    error: Option<String>,
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
            audio_tracks: None,
            audio_playback: None,
            bulk_queue: None,
            bulk_loader: None,
            bulk_loaded_cursor: None,
            bulk_suppress_cascade: false,
            pending_hash_redump_id: None,
            pending_export_url: None,
            bulk_banner_bottom_y: None,
            broken_cue_prompt: None,
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
        // Bulk mode commits to the queue's pre-decided match — don't let the
        // exact/fuzzy cascade overwrite it.
        if self.bulk_suppress_cascade {
            return;
        }
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
                    self.log(LogLevel::Info, "Redump: no exact match, trying fuzzy");
                    self.run_fuzzy(&conn, info);
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

    /// Run fuzzy matching after the exact cascade missed. Attaches a ranked
    /// candidate list in-place and logs a one-line summary. During the initial
    /// data-collection phase the full list is surfaced regardless of score.
    fn run_fuzzy(&mut self, conn: &rusqlite::Connection, info: &mut DiscInfo) {
        let cfg = &crate::config::get_config().fuzzy_match;
        match crate::db::fuzzy_from_disc(conn, info, cfg, true) {
            Ok(candidates) => {
                if candidates.is_empty() {
                    self.log(LogLevel::Info, "Fuzzy: no candidates above floor");
                } else {
                    let top = &candidates[0];
                    self.log(
                        LogLevel::Info,
                        format!(
                            "Fuzzy: {} candidate(s); top {} [#{}] {:.2} ({})",
                            candidates.len(),
                            top.title,
                            top.redump_id,
                            top.score,
                            top.match_reason,
                        ),
                    );
                }
                info.fuzzy_matches = Some(candidates);
            }
            Err(e) => {
                log::warn!("Fuzzy search failed: {e}");
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
    /// Render the CD-DA track list with play/stop controls for a CHD that has
    /// audio tracks. No-op when there are no cached audio tracks. Takes the disc
    /// path explicitly so the caller can keep `self.disc_info` borrowed while we
    /// borrow `self` mutably here.
    fn render_audio_player(&mut self, ui: &mut egui::Ui) {
        let has_audio = self
            .audio_tracks
            .as_ref()
            .map(|tracks| tracks.iter().any(|t| t.is_audio))
            .unwrap_or(false);
        if !has_audio {
            return;
        }

        // Reap a finished/failed job so the play buttons re-enable.
        let mut playback_error: Option<String> = None;
        if let Some(pb) = self.audio_playback.as_ref() {
            match pb.state() {
                super::audio::PlaybackState::Failed(e) => {
                    playback_error = Some(e);
                    self.audio_playback = None;
                }
                super::audio::PlaybackState::Finished => {
                    self.audio_playback = None;
                }
                _ => {}
            }
        }

        // Snapshot the active job so we don't hold a borrow across the UI loop.
        let active = self
            .audio_playback
            .as_ref()
            .map(|pb| (pb.track(), pb.state()));

        ui.separator();
        ui.label(egui::RichText::new("Audio Tracks").strong());

        let mut to_play: Option<u32> = None;
        let mut do_stop = false;

        if let Some(tracks) = self.audio_tracks.as_ref() {
            for t in tracks.iter().filter(|t| t.is_audio) {
                ui.horizontal(|ui| {
                    let is_this = active.as_ref().map(|(n, _)| *n == t.number).unwrap_or(false);
                    if is_this {
                        let status = match active.as_ref().map(|(_, s)| s) {
                            Some(super::audio::PlaybackState::Preparing) => "preparing…",
                            Some(super::audio::PlaybackState::Playing) => "playing",
                            _ => "…",
                        };
                        if ui.button("⏹ Stop").clicked() {
                            do_stop = true;
                        }
                        ui.label(format!("Track {} — {}", t.number, status));
                    } else {
                        let busy = active.is_some();
                        if ui.add_enabled(!busy, egui::Button::new("▶")).clicked() {
                            to_play = Some(t.number);
                        }
                        ui.label(format!("Track {} ({})", t.number, t.duration_mmss()));
                    }
                });
            }
        }

        if let Some(err) = playback_error {
            ui.colored_label(egui::Color32::RED, format!("Playback failed: {err}"));
        }

        if do_stop {
            // Drop stops playback.
            self.audio_playback = None;
        }
        if let Some(track) = to_play {
            if let Some(disc_path) = self.selected_path.clone() {
                self.audio_playback = Some(super::audio::AudioPlayback::start(disc_path, track));
            }
        }

        // While a job is active, keep repainting so the status label and the
        // auto-reap stay current.
        if self.audio_playback.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

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
                    // Hashing's redump-cascade benefit is mostly for ISO9660
                    // data discs that redump catalogs deeply. For HFS / HFS+
                    // discs the hash tier rarely helps even when it succeeds,
                    // and the underlying CHD/CUE path frequently rejects them
                    // outright. Surface that context so the warning isn't
                    // mysterious.
                    let suffix = match self
                        .disc_info
                        .as_ref()
                        .and_then(|r| r.as_ref().ok())
                        .map(|i| i.filesystem)
                    {
                        Some(FilesystemType::Hfs) | Some(FilesystemType::HfsPlus) => {
                            " (normal for HFS/HFS+ discs — redump doesn't catalog Mac \
                             hashes deeply, so the hash tier wouldn't add much anyway)"
                        }
                        _ => "",
                    };
                    log::warn!("Hashing failed: {e}{suffix}");
                    self.log(
                        LogLevel::Warning,
                        format!("Hashing failed: {e}{suffix}"),
                    );
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

        let head = matches[0].clone();
        let head_summary = format!(
            "Redump hash match: {} [#{}] via {:?}",
            head.title, head.redump_id, head.matched_via
        );

        // In bulk mode the queue's match is authoritative. Record the
        // disagreement (if any) on the active queue item so it lands in the
        // done log when the user saves/skips, but don't disturb the UI.
        if let Some(queue) = self.bulk_queue.as_mut() {
            if let Some(item) = queue.current() {
                let queue_id = item.best.redump_id;
                if queue_id != Some(head.redump_id) {
                    self.log(
                        LogLevel::Info,
                        format!(
                            "Bulk: hash points to #{} (queue locked to #{:?}); recorded as disagreement",
                            head.redump_id, queue_id
                        ),
                    );
                } else {
                    self.log(LogLevel::Info, "Bulk: hash confirms queue match");
                }
            }
            self.pending_hash_redump_id = Some(head.redump_id);
            return;
        }

        if let Some(Ok(info)) = self.disc_info.as_mut() {
            info.redump_matches = Some(matches);
            // Hash hit beats any fuzzy candidates from the earlier pass —
            // drop the stale list so the UI doesn't show both.
            info.fuzzy_matches = None;
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
                        self.log(LogLevel::Info, "Lookup DB is up to date");
                    }
                    crate::db::UpdateOutcome::Updated { local_path, .. } => {
                        self.log(
                            LogLevel::Success,
                            format!("Lookup DB updated: {}", local_path.display()),
                        );
                    }
                    crate::db::UpdateOutcome::OfflineUsingCached { error, .. } => {
                        self.log(
                            LogLevel::Warning,
                            format!("Lookup DB update skipped (offline): {error}"),
                        );
                    }
                    crate::db::UpdateOutcome::OfflineNoCache { error } => {
                        self.log(
                            LogLevel::Warning,
                            format!("Lookup DB unavailable (offline, no cache): {error}"),
                        );
                    }
                }
                self.db_update_done = true;
                self.db_update_receiver = None;
            }
            Ok(Err(e)) => {
                self.log(LogLevel::Error, format!("Lookup DB update failed: {e}"));
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
        // Pre-flight: detect cue files whose BINs are missing and surface a
        // "delete cue?" prompt before any of the rest of the load runs. The
        // prompt is modal — the disc isn't loaded until the user resolves
        // it (or, in bulk mode, the auto-skip timeout fires).
        let scan = crate::disc::scan_cue_references(&path);
        if !scan.missing.is_empty()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("cue"))
                .unwrap_or(false)
        {
            self.broken_cue_prompt = Some(BrokenCuePrompt {
                cue_path: path.clone(),
                missing: scan.missing,
                total_refs: scan.total_refs,
                in_bulk: self.bulk_queue.is_some(),
                opened_at: std::time::Instant::now(),
            });
            self.log(
                LogLevel::Warning,
                format!(
                    "Broken cue detected: {} (missing data file(s))",
                    path.display()
                ),
            );
            return;
        }

        self.log(LogLevel::Info, format!("Processing: {}", path.display()));
        self.selected_path = Some(path.clone());

        // Clear previous search state
        self.search_query_text.clear();
        self.manual_url.clear();
        self.search_results.clear();
        self.selected_image_index = None;
        self.preview_texture = None;
        self.preview_url = None;
        self.audio_tracks = None;
        self.audio_playback = None; // Drop stops any in-flight playback.

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
                // Cache the CD audio track list for CHDs so the disc panel can
                // offer per-track playback without re-opening the CHD each frame.
                self.audio_tracks = if info.format == DiscFormat::Chd {
                    crate::disc::chd_audio::read_tracks(&info.path).ok()
                } else {
                    None
                };
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
                        fuzzy_matches: None,
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

    /// Prompt the user for a `fuzzy_scan --queue` JSON file and stage it in
    /// the loader dialog. The actual queue activation happens when they
    /// click "Start" in the dialog.
    fn open_bulk_job_picker(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Queue (JSON or CSV)", &["json", "csv"])
            .add_filter("Queue JSON", &["json"])
            .add_filter("fuzzy_scan CSV", &["csv"])
            .add_filter("All Files", &["*"])
            .pick_file()
        else {
            return;
        };

        let filter = super::bulk::QueueFilter::default();
        let (items, error) = match super::bulk::parse_queue_file(&path, &filter) {
            Ok(items) => (items, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let resumed_count = count_resumed(&path, &items);

        self.bulk_loader = Some(BulkLoaderDialog {
            path,
            item_count: items.len(),
            resumed_count,
            reprocess_existing: false,
            filter: filter.clone(),
            last_filter: filter,
            error,
        });
    }

    /// Re-parse the staged queue file under the dialog's current filter
    /// settings so the displayed item count reflects what the user will get
    /// when they click Start. Called when the include-fuzzy toggle or the
    /// confidence slider changes value.
    fn refresh_bulk_loader_counts(&mut self) {
        let Some(dialog) = self.bulk_loader.as_mut() else {
            return;
        };
        if filters_eq(&dialog.filter, &dialog.last_filter) {
            return;
        }
        match super::bulk::parse_queue_file(&dialog.path, &dialog.filter) {
            Ok(items) => {
                dialog.item_count = items.len();
                dialog.resumed_count = count_resumed(&dialog.path, &items);
                dialog.error = None;
            }
            Err(e) => {
                dialog.item_count = 0;
                dialog.resumed_count = 0;
                dialog.error = Some(e);
            }
        }
        dialog.last_filter = dialog.filter.clone();
    }

    /// Activate the queue using the staged loader dialog. Closes the dialog.
    fn start_bulk_job(&mut self) {
        let Some(dialog) = self.bulk_loader.take() else {
            return;
        };
        match super::bulk::BulkQueue::load(
            dialog.path.clone(),
            dialog.reprocess_existing,
            &dialog.filter,
        ) {
            Ok(queue) => {
                self.log(
                    LogLevel::Success,
                    format!(
                        "Bulk job loaded: {} item(s), {} already done",
                        queue.total(),
                        queue.finished_count()
                    ),
                );
                self.bulk_queue = Some(queue);
            }
            Err(e) => {
                self.log(LogLevel::Error, format!("Bulk job load failed: {e}"));
            }
        }
    }

    /// Exit bulk mode without affecting the sidecar log.
    fn close_bulk_job(&mut self) {
        if self.bulk_queue.is_some() {
            self.log(LogLevel::Info, "Bulk job closed".to_string());
        }
        self.bulk_queue = None;
        self.bulk_loaded_cursor = None;
        self.bulk_suppress_cascade = false;
        self.pending_hash_redump_id = None;
    }

    /// Clear all per-disc state so the central panel returns to the
    /// "no file selected" view. Used when bulk mode finishes the last
    /// item — the prior disc shouldn't stay loaded as if the user picked
    /// it manually. Also cancels any in-flight hashing/search/preview.
    fn unload_disc(&mut self) {
        self.cancel_hashing();
        self.selected_path = None;
        self.disc_info = None;
        self.search_query_text.clear();
        self.manual_url.clear();
        self.search_results.clear();
        self.selected_image_index = None;
        self.preview_texture = None;
        self.preview_url = None;
        self.preview_error = None;
        self.show_search_window = false;
        self.browse_view.clear();
        self.show_browse_window = false;
        self.audio_tracks = None;
        self.audio_playback = None;
    }

    /// Per-frame driver for bulk mode. If the cursor advanced to a new item,
    /// either auto-skip it (existing art + reprocess off) or load the disc
    /// and inject the queue's chosen match. After loading, the artwork
    /// search is auto-triggered for exact matches.
    fn tick_bulk(&mut self) {
        let Some(queue) = self.bulk_queue.as_ref() else {
            return;
        };
        if queue.is_complete() {
            // First tick after the final item resolved: unload the disc so
            // the central panel doesn't keep showing the last-processed
            // file. The banner stays up (with the green "complete" notice)
            // until the user clicks Exit bulk.
            if self.bulk_loaded_cursor.is_some() {
                self.unload_disc();
                self.bulk_loaded_cursor = None;
            }
            return;
        }
        let cursor = queue.cursor;
        if Some(cursor) == self.bulk_loaded_cursor {
            return;
        }

        // Snapshot what we need; later mutating self requires releasing the
        // queue borrow.
        let Some(item) = queue.current() else {
            return;
        };
        let path = std::path::PathBuf::from(&item.file);
        let has_existing = item.has_existing_art;
        let reprocess = queue.reprocess_existing;
        let redump_id = item.best.redump_id;
        let match_type = item.best.match_type.clone();
        let display_title = item.best.title.clone();
        let best_score = item.best.score;
        let alt_count = item.alternates.len();
        let already_resolved = queue
            .statuses
            .get(cursor)
            .map(|s| *s != super::bulk::ItemStatus::Pending)
            .unwrap_or(false);
        log::debug!(
            "bulk: tick advance cursor={} file={} best=#{:?} \"{}\" ({}, score={:?}) alts={} \
             has_existing_art={} reprocess_existing={} already_resolved={}",
            cursor, path.display(), redump_id, display_title, match_type, best_score,
            alt_count, has_existing, reprocess, already_resolved,
        );

        // Existing-art auto-skip — but only for items that haven't been
        // resolved yet. If the user explicitly went Back to a Saved /
        // Skipped / existing-art item, load it so they can redo the
        // choice; otherwise Back is useless on those items.
        if has_existing && !reprocess && !already_resolved {
            self.log(
                LogLevel::Info,
                format!("Bulk: skipping (existing art): {}", path.display()),
            );
            self.record_bulk_done("existing-art", None);
            // Don't mark this cursor as loaded; let advance kick us to the
            // next pending item on the next tick.
            return;
        }

        if already_resolved {
            self.log(
                LogLevel::Info,
                format!(
                    "Bulk: reopening previously-resolved item {} for re-processing",
                    path.display()
                ),
            );
        }

        // Reset per-item transient state before kicking off the load.
        self.pending_hash_redump_id = None;
        self.bulk_loaded_cursor = Some(cursor);
        self.bulk_suppress_cascade = true;
        self.process_file(path.clone());
        self.bulk_suppress_cascade = false;

        // Inject the queue's chosen match in place of whatever the disc-read
        // path would have produced. We pass the queue's best Record as a
        // fallback so a missing entry in the local DB (e.g. CSV built
        // against a different snapshot) still seeds redump_matches with
        // the title — otherwise the search query would silently revert to
        // the filename.
        let queue_best = self
            .bulk_queue
            .as_ref()
            .and_then(|q| q.current())
            .map(|it| it.best.clone());
        if let Some(rid) = redump_id {
            self.inject_bulk_match(rid, &display_title, queue_best.as_ref());
        }

        // For exact matches the queue is trusted enough to auto-trigger the
        // artwork search. Fuzzy matches go through a confirm step in the
        // next phase (Phase 4); for now they also auto-search so the flow
        // is exercisable.
        self.update_search_query_from_disc();
        if !self.search_query_text.is_empty() {
            let q = self.search_query_text.clone();
            self.start_search(&q);
        }
        let _ = match_type; // reserved for the fuzzy-confirm step in Phase 4.
    }

    /// Replace `disc_info.redump_matches` with a single RedumpMatch fetched
    /// by `redump_id`, so the UI reflects the queue's pre-decided answer
    /// without re-running the cascade. If the local DB doesn't have the
    /// id (e.g. it's older than the snapshot the CSV was built against),
    /// synthesize a minimal match from `queue_best` so the search query
    /// still uses the correct title.
    fn inject_bulk_match(
        &mut self,
        redump_id: i64,
        fallback_title: &str,
        queue_best: Option<&super::bulk::Record>,
    ) {
        let result = (|| -> Result<Option<crate::db::RedumpMatch>, String> {
            let mgr =
                crate::db::DatabaseManager::new().map_err(|e| e.to_string())?;
            let conn = mgr.open().map_err(|e| e.to_string())?;
            crate::db::by_redump_id(&conn, redump_id).map_err(|e| e.to_string())
        })();

        let m = match result {
            Ok(Some(m)) => {
                log::debug!(
                    "bulk: inject local DB hit #{} \"{}\" matched_via={:?}",
                    m.redump_id, m.title, m.matched_via
                );
                m
            }
            Ok(None) => {
                self.log(
                    LogLevel::Warning,
                    format!(
                        "Bulk: redump #{redump_id} ({fallback_title}) not found in local DB; \
                         using queue's title for search"
                    ),
                );
                let Some(rec) = queue_best else { return };
                log::debug!(
                    "bulk: inject synthesized from queue Record #{:?} \"{}\"",
                    rec.redump_id, rec.title
                );
                synth_match_from_record(rec)
            }
            Err(e) => {
                self.log(
                    LogLevel::Warning,
                    format!("Bulk: failed to fetch redump #{redump_id}: {e}"),
                );
                let Some(rec) = queue_best else { return };
                log::debug!(
                    "bulk: inject synthesized after DB error from queue Record #{:?} \"{}\"",
                    rec.redump_id, rec.title
                );
                synth_match_from_record(rec)
            }
        };

        let log_line = format!(
            "Bulk: locked match {} [#{}] via {:?}",
            m.title, m.redump_id, m.matched_via
        );
        if let Some(Ok(info)) = self.disc_info.as_mut() {
            info.redump_matches = Some(vec![m]);
            info.fuzzy_matches = None;
        }
        self.log(LogLevel::Success, log_line);
    }

    /// Bulk-mode keyboard shortcuts:
    ///   Enter — save the currently focused image (or re-trigger the save
    ///           button) — handled by the existing Save button path.
    ///   S     — skip the current item (record as 'skipped', advance).
    ///   B     — go back to the previous item (no log entry; cursor only).
    ///   Esc   — exit bulk mode entirely.
    /// Hotkeys are no-ops outside bulk mode and when the loader dialog is
    /// open. We also skip keys consumed by a focused text input so the
    /// search-query field stays editable.
    fn handle_bulk_hotkeys(&mut self, ctx: &egui::Context) {
        if self.bulk_queue.is_none() {
            return;
        }
        if self.bulk_loader.is_some() {
            return;
        }
        let text_focused = ctx.memory(|m| m.focused().is_some());
        if text_focused {
            return;
        }

        let (skip, back, exit, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::B),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Enter),
            )
        });

        if exit {
            self.close_bulk_job();
            return;
        }
        if skip {
            self.log(LogLevel::Info, "Bulk: skipped");
            self.record_bulk_done("skipped", None);
            return;
        }
        if back {
            if let Some(queue) = self.bulk_queue.as_mut() {
                queue.back();
            }
            self.bulk_loaded_cursor = None;
            self.pending_hash_redump_id = None;
            return;
        }
        if enter {
            // Save currently-previewed image, if any. The bulk advance
            // happens inside poll_export on success.
            if self.export_in_progress {
                return;
            }
            let url = self.preview_url.clone();
            let path = self
                .selected_path
                .as_ref()
                .map(|p| generate_output_path(p));
            if let (Some(url), Some(path)) = (url, path) {
                self.start_export(&url, &path);
            } else {
                self.log(
                    LogLevel::Info,
                    "Bulk: Enter pressed but no image is selected for preview",
                );
            }
        }
    }

    /// Append a done-log entry for the current bulk item and advance the
    /// cursor. Caller chooses the status string (saved/skipped/existing-art).
    fn record_bulk_done(&mut self, status: &str, image_url: Option<String>) {
        // Build the entry while holding the queue borrow; release it before
        // we touch self.log.
        let (write_err, item_status_for_advance) = {
            let Some(queue) = self.bulk_queue.as_mut() else {
                return;
            };
            let Some(item) = queue.current() else {
                return;
            };
            let queue_redump_id = item.best.redump_id;
            let file = item.file.clone();
            let hash_redump_id = self
                .pending_hash_redump_id
                .filter(|h| Some(*h) != queue_redump_id);
            let entry = super::bulk::DoneEntry {
                file,
                status: status.to_string(),
                queue_redump_id,
                hash_redump_id,
                image_url,
                ts: super::bulk::now_iso8601(),
            };
            let item_status = match status {
                "saved" => super::bulk::ItemStatus::Saved,
                "skipped" => super::bulk::ItemStatus::Skipped,
                "existing-art" => super::bulk::ItemStatus::ExistingArt,
                _ => super::bulk::ItemStatus::Skipped,
            };
            let err = queue.record(entry, item_status).err();
            queue.advance();
            (err, item_status)
        };

        if let Some(e) = write_err {
            self.log(LogLevel::Error, format!("Bulk: done-log write failed: {e}"));
        }
        let _ = item_status_for_advance;
        self.bulk_loaded_cursor = None;
        self.pending_hash_redump_id = None;
    }

    /// Render the broken-cue confirmation modal when one is staged. In bulk
    /// mode shows a countdown that auto-dismisses with "Keep" after
    /// `BULK_AUTO_SKIP_SECS` so a long-running queue isn't stuck behind a
    /// prompt the user isn't around to answer.
    fn render_broken_cue_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.broken_cue_prompt.as_ref() else {
            return;
        };
        let cue = prompt.cue_path.clone();
        let missing = prompt.missing.clone();
        let total = prompt.total_refs;
        let in_bulk = prompt.in_bulk;
        let elapsed = prompt.opened_at.elapsed().as_secs();
        let remaining = BULK_AUTO_SKIP_SECS.saturating_sub(elapsed);

        let mut delete_clicked = false;
        let mut keep_clicked = false;

        egui::Window::new("Broken CUE file")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(cue.display().to_string()).strong());
                ui.add_space(6.0);
                ui.label(format!(
                    "{} of {} referenced data file(s) cannot be found:",
                    missing.len(),
                    total,
                ));
                ui.add_space(4.0);
                for name in &missing {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("  · {name}"),
                    );
                }
                ui.add_space(8.0);
                ui.label("Delete the cue file? (The cue can't be used without its data.)");

                if in_bulk {
                    ui.add_space(4.0);
                    ui.weak(format!(
                        "Auto-skipping (keep, don't delete) in {remaining}s …"
                    ));
                    // Pump the frame so the countdown ticks.
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("Delete cue").color(egui::Color32::LIGHT_RED),
                        ))
                        .clicked()
                    {
                        delete_clicked = true;
                    }
                    if ui.button("Keep (skip)").clicked() {
                        keep_clicked = true;
                    }
                });
            });

        // Bulk-mode auto-skip after the timeout. Conservative default:
        // never auto-delete — only auto-keep.
        if in_bulk && remaining == 0 {
            keep_clicked = true;
        }

        if delete_clicked {
            self.resolve_broken_cue(true);
        } else if keep_clicked {
            self.resolve_broken_cue(false);
        }
    }

    /// Apply the user's decision for the active broken-cue prompt. `delete`
    /// removes the cue file from disk and logs the result; `false` keeps it.
    /// In bulk mode also records the queue item as `skipped` (with a
    /// distinct reason in the log) and advances the cursor.
    fn resolve_broken_cue(&mut self, delete: bool) {
        let Some(prompt) = self.broken_cue_prompt.take() else {
            return;
        };
        let in_bulk = prompt.in_bulk;
        let cue = prompt.cue_path;

        if delete {
            match std::fs::remove_file(&cue) {
                Ok(()) => self.log(
                    LogLevel::Success,
                    format!("Deleted broken cue: {}", cue.display()),
                ),
                Err(e) => self.log(
                    LogLevel::Error,
                    format!("Could not delete {}: {e}", cue.display()),
                ),
            }
        } else {
            self.log(
                LogLevel::Info,
                format!("Kept broken cue (skipped): {}", cue.display()),
            );
        }

        if in_bulk {
            // Either path skips this queue item — record + advance so the
            // bulk run keeps moving.
            self.record_bulk_done("skipped", None);
        }
    }

    /// Render the bulk loader modal when one is staged.
    fn render_bulk_loader(&mut self, ctx: &egui::Context) {
        let mut start_clicked = false;
        let mut cancel_clicked = false;
        let mut filter_changed = false;

        if let Some(dialog) = self.bulk_loader.as_mut() {
            egui::Window::new("Open Bulk Job")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("File: {}", dialog.path.display()));
                    ui.add_space(6.0);
                    if let Some(err) = &dialog.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    } else {
                        ui.label(format!("Items: {}", dialog.item_count));
                        if dialog.resumed_count > 0 {
                            ui.label(format!(
                                "Already done (will skip): {}",
                                dialog.resumed_count
                            ));
                        }
                    }
                    ui.add_space(8.0);
                    ui.checkbox(
                        &mut dialog.reprocess_existing,
                        "Reprocess discs that already have artwork",
                    );

                    ui.add_space(4.0);
                    let prev_include = dialog.filter.include_fuzzy;
                    if ui
                        .checkbox(
                            &mut dialog.filter.include_fuzzy,
                            "Include partially-matched discs (fuzzy)",
                        )
                        .changed()
                    {
                        filter_changed = true;
                    }
                    if dialog.filter.include_fuzzy {
                        ui.horizontal(|ui| {
                            ui.label("Minimum confidence:");
                            let resp = ui.add(
                                egui::Slider::new(
                                    &mut dialog.filter.fuzzy_min_score,
                                    0.60..=0.95,
                                )
                                .fixed_decimals(2)
                                .step_by(0.01),
                            );
                            if resp.drag_stopped() || resp.lost_focus() {
                                filter_changed = true;
                            }
                        });
                        ui.weak(
                            "0.60 = fuzzy floor (noisy). 0.85 is selective without rejecting \
                             genuine matches. 0.90+ is exact-like confidence.",
                        );
                    }
                    if !prev_include && dialog.filter.include_fuzzy {
                        filter_changed = true;
                    }

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let can_start = dialog.error.is_none() && dialog.item_count > 0;
                        if ui
                            .add_enabled(can_start, egui::Button::new("Start"))
                            .clicked()
                        {
                            start_clicked = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_clicked = true;
                        }
                    });
                });
        }

        if filter_changed {
            self.refresh_bulk_loader_counts();
        }
        if start_clicked {
            self.start_bulk_job();
        } else if cancel_clicked {
            self.bulk_loader = None;
        }
    }

    /// Inline top banner shown above the central panel content when bulk
    /// mode is active. Shows progress, the current item, a short preview
    /// of upcoming items, and the action buttons (Skip / Back / Exit).
    fn render_bulk_banner(&mut self, ui: &mut egui::Ui) {
        let Some(queue) = self.bulk_queue.as_ref() else {
            self.bulk_banner_bottom_y = None;
            return;
        };
        let total = queue.total();
        let done = queue.finished_count();
        let current = queue.current().map(|it| it.file.clone());
        let title = queue
            .current()
            .map(|it| it.best.title.clone())
            .unwrap_or_default();
        let cursor = queue.cursor;
        let complete = queue.is_complete();

        // Snapshot the next ~3 pending items for the "Up next" strip.
        let upcoming: Vec<(usize, String, String)> = queue
            .items
            .iter()
            .enumerate()
            .skip(cursor + 1)
            .filter(|(idx, _)| queue.statuses[*idx] == super::bulk::ItemStatus::Pending)
            .take(3)
            .map(|(idx, it)| {
                let stem = std::path::Path::new(&it.file)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&it.file)
                    .to_string();
                (idx, stem, it.best.title.clone())
            })
            .collect();

        let mut exit_clicked = false;
        let mut skip_clicked = false;
        let mut back_clicked = false;
        let frame_resp = egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .show(ui, |ui| {
                // Row 1: status + current item. The current item gets a
                // marquee so a long filename doesn't fight the layout —
                // it scrolls inside a fixed-width region instead of
                // wrapping or being clipped.
                ui.horizontal(|ui| {
                    ui.heading(if complete {
                        "Bulk job — complete"
                    } else {
                        "Bulk mode"
                    });
                    ui.separator();
                    ui.label(format!("{done} / {total} done"));
                    ui.separator();
                    if let Some(file) = current {
                        let stem = std::path::Path::new(&file)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(&file);
                        let line = if title.is_empty() {
                            format!("[{}] {}", cursor + 1, stem)
                        } else {
                            format!("[{}] {}  →  {}", cursor + 1, stem, title)
                        };
                        let available = ui.available_width().max(120.0);
                        marquee_label(ui, &line, available);
                    }
                });

                ui.add_space(2.0);

                // Row 2: controls + upcoming on the same line. Controls go
                // first so they're always reachable; the upcoming list
                // takes whatever space is left and marquees if too long.
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(cursor > 0, egui::Button::new("Back"))
                        .on_hover_text("B — return to previous item")
                        .clicked()
                    {
                        back_clicked = true;
                    }
                    if ui
                        .add_enabled(!complete, egui::Button::new("Skip"))
                        .on_hover_text("S — record as skipped and advance")
                        .clicked()
                    {
                        skip_clicked = true;
                    }
                    if ui
                        .button("Exit bulk")
                        .on_hover_text("Esc")
                        .clicked()
                    {
                        exit_clicked = true;
                    }

                    ui.add_space(8.0);
                    ui.separator();

                    if !upcoming.is_empty() {
                        ui.weak("Up next:");
                        let line = upcoming
                            .iter()
                            .map(|(_, stem, t)| {
                                if t.is_empty() {
                                    stem.clone()
                                } else {
                                    format!("{stem} → {t}")
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("  ·  ");
                        let avail = ui.available_width().max(120.0);
                        marquee_label(ui, &line, avail);
                    } else if !complete {
                        ui.weak("Last item in queue.");
                    } else {
                        ui.colored_label(
                            egui::Color32::LIGHT_GREEN,
                            "All items processed — click Exit bulk to return to single-disc mode.",
                        );
                    }
                });
            });

        self.bulk_banner_bottom_y = Some(frame_resp.response.rect.bottom());

        ui.add_space(4.0);

        if exit_clicked {
            self.close_bulk_job();
        }
        if skip_clicked {
            self.log(LogLevel::Info, "Bulk: skipped");
            self.record_bulk_done("skipped", None);
        }
        if back_clicked {
            if let Some(queue) = self.bulk_queue.as_mut() {
                queue.back();
            }
            self.bulk_loaded_cursor = None;
            self.pending_hash_redump_id = None;
        }
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

    /// Start exporting artwork. Reads the current disc's `disc_number` from
    /// the parsed filename so discs 2+ pick up the "Disc N" overlay badge.
    fn start_export(&mut self, image_url: &str, output_path: &str) {
        let url = image_url.to_string();
        let path = output_path.to_string();
        let (tx, rx) = mpsc::channel();

        self.export_in_progress = true;
        self.export_receiver = Some(rx);
        self.pending_export_url = Some(url.clone());

        let (disc_number, disc_total) = self.current_disc_marker();

        self.log(LogLevel::Info, format!("Downloading and converting to {}", path));

        thread::spawn(move || {
            let settings = ExportSettings::default();
            let result = export_artwork_from_url_with_disc(
                &url, &path, &settings, disc_number, disc_total,
            );
            let _ = tx.send(result);
        });
    }

    /// After a successful save, re-export the same image URL to every
    /// multi-disc sibling of the current disc — same source, but each one
    /// stamped with its own disc-number badge. In bulk mode, also marks
    /// those siblings as `saved` in the queue + done log so we don't
    /// prompt for them again. In single-disc mode this is "auto-fill the
    /// rest of the set" after one user choice.
    ///
    /// Synchronous: this runs on the UI thread after the main download has
    /// already cached the image. Each sibling re-fetches the same URL — a
    /// minor inefficiency we accept rather than refactoring the export
    /// pipeline to take pre-fetched bytes.
    fn apply_to_siblings(&mut self, image_url: &str) {
        let Some(selected) = self.selected_path.clone() else {
            return;
        };

        // Build the sibling work-list from whichever source applies:
        //   1. In bulk mode, the queue's same-set neighbors that haven't
        //      been processed yet.
        //   2. In single-disc mode, a directory scan.
        let siblings: Vec<(std::path::PathBuf, crate::disc::set_membership::DiscMarker)> =
            if self.bulk_queue.is_some() {
                self.queue_siblings_for(&selected)
            } else {
                crate::disc::set_membership::siblings_in_dir(&selected)
                    .into_iter()
                    .map(|s| (s.path, s.marker))
                    .collect()
            };

        if siblings.is_empty() {
            return;
        }

        let total_from_match = self
            .disc_info
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .and_then(|info| {
                info.redump_matches
                    .as_ref()
                    .and_then(|ms| ms.first())
                    .and_then(|m| crate::disc::set_membership::parse_disc_total(&m.title))
            });

        self.log(
            LogLevel::Info,
            format!(
                "Applying same artwork to {} multi-disc sibling(s) with per-disc badges",
                siblings.len()
            ),
        );

        let settings = ExportSettings::default();
        for (sib_path, sib_marker) in &siblings {
            // For numbered markers, swap in the total hint we got from the
            // redump title (if any) so "Disc 2" becomes "Disc 2/3".
            let badge_marker = match sib_marker {
                crate::disc::set_membership::DiscMarker::Numbered { number, total } => {
                    crate::disc::set_membership::DiscMarker::Numbered {
                        number: *number,
                        total: total.or(total_from_match),
                    }
                }
                role @ crate::disc::set_membership::DiscMarker::Role(_) => role.clone(),
            };
            let label = badge_marker.badge_label();
            let out_path = generate_output_path(sib_path);
            let result = export_artwork_from_url_with_label(
                image_url,
                &out_path,
                &settings,
                label.as_deref(),
            );
            let pretty = label.clone().unwrap_or_else(|| "(unbadged)".into());
            match result {
                Ok(_) => self.log(
                    LogLevel::Success,
                    format!("  sibling {pretty}: saved to {out_path}"),
                ),
                Err(e) => self.log(
                    LogLevel::Warning,
                    format!("  sibling {pretty}: failed ({e})"),
                ),
            }
        }

        // For bulk mode, mark these sibling queue items as done so the
        // user doesn't have to re-pick artwork for each.
        if let Some(queue) = self.bulk_queue.as_mut() {
            let now = super::bulk::now_iso8601();
            for (sib_path, _) in &siblings {
                let sib_file = sib_path.to_string_lossy().to_string();
                let Some(idx) = queue.items.iter().position(|it| it.file == sib_file) else {
                    continue;
                };
                let entry = super::bulk::DoneEntry {
                    file: sib_file,
                    status: "saved".to_string(),
                    queue_redump_id: queue.items[idx].best.redump_id,
                    hash_redump_id: None,
                    image_url: Some(image_url.to_string()),
                    ts: now.clone(),
                };
                if let Err(e) =
                    queue.record_at(idx, entry, super::bulk::ItemStatus::Saved)
                {
                    log::warn!("sibling done-log write failed: {e}");
                }
            }
        }
    }

    /// Find unfinished queue items whose disc file shares a set key with
    /// `selected`. Returns (path, marker) pairs ready for export — the
    /// marker decides what label (if any) lands on the badge.
    fn queue_siblings_for(
        &self,
        selected: &std::path::Path,
    ) -> Vec<(std::path::PathBuf, crate::disc::set_membership::DiscMarker)> {
        let Some(queue) = self.bulk_queue.as_ref() else {
            return Vec::new();
        };
        let Some(my_stem) = selected.file_stem().and_then(|s| s.to_str()) else {
            return Vec::new();
        };
        let my_key = crate::disc::set_membership::set_key(my_stem);
        let mut out = Vec::new();
        for (idx, item) in queue.items.iter().enumerate() {
            if queue.statuses[idx] != super::bulk::ItemStatus::Pending {
                continue;
            }
            let path = std::path::PathBuf::from(&item.file);
            if path == selected {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if crate::disc::set_membership::set_key(stem) != my_key {
                continue;
            }
            let Some(marker) = crate::disc::set_membership::disc_marker_from_stem(stem)
                .or_else(|| crate::disc::set_membership::disc_marker_from_stem(&item.best.title))
            else {
                continue;
            };
            out.push((path, marker));
        }
        out
    }

    /// Snapshot the current disc's `(Disc N)` marker for export-time badging.
    /// Returns `(number, total)`. `total` is unknown unless the redump title
    /// includes "Disc N of M" — common enough on multi-disc sets that we try.
    fn current_disc_marker(&self) -> (Option<u32>, Option<u32>) {
        let Some(Ok(info)) = self.disc_info.as_ref() else {
            return (None, None);
        };
        let n = info.parsed_filename.disc_number;
        let total = info
            .redump_matches
            .as_ref()
            .and_then(|ms| ms.first())
            .and_then(|m| crate::disc::set_membership::parse_disc_total(&m.title));
        (n, total)
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
                    self.show_search_window = false;
                    let saved_url = self.pending_export_url.take();
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

                    // Multi-disc siblings get the same image with their own
                    // disc-number badge. Done before bulk-advance so the
                    // queue cursor doesn't move past siblings we still need
                    // to process.
                    if let Some(url) = saved_url.clone() {
                        self.apply_to_siblings(&url);
                    }

                    // If we're in bulk mode, record the save and advance.
                    if self.bulk_queue.is_some() {
                        self.record_bulk_done("saved", saved_url);
                    }
                }
                Ok(Err(e)) => {
                    self.export_in_progress = false;
                    self.export_receiver = None;
                    self.pending_export_url = None;
                    self.log(LogLevel::Error, format!("Export failed: {}", e));
                }
                Err(TryRecvError::Empty) => {
                    // Still exporting
                }
                Err(TryRecvError::Disconnected) => {
                    self.export_in_progress = false;
                    self.export_receiver = None;
                    self.pending_export_url = None;
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

/// Single-line scrolling text. If `text` fits in `max_width`, renders as a
/// plain label; otherwise the text slides continuously to the left, with a
/// duplicate trailing copy so the loop is seamless. Hovering the strip shows
/// the full text in a tooltip for users who'd rather read than wait.
fn marquee_label(ui: &mut egui::Ui, text: &str, max_width: f32) {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font_id, color);
    let natural_w = galley.size().x;
    let h = galley.size().y;
    let visible_w = max_width.min(natural_w.max(1.0));
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(visible_w.max(40.0), h),
        egui::Sense::hover(),
    );

    if natural_w <= visible_w {
        ui.painter()
            .galley(rect.left_top(), galley, color);
        return;
    }

    // Clip drawing to the allocated rect so the looping copy doesn't bleed
    // into neighboring widgets.
    let painter = ui.painter_at(rect);

    // 30 px gap between the trailing edge of one copy and the leading edge
    // of the next; tuned to feel airy on Roboto-like fonts without leaving
    // a long blank gap.
    let gap = 30.0_f32;
    let cycle = natural_w + gap;
    // 40 px/sec is slow enough to read a 40-char title in ~10s, fast enough
    // that you don't feel stuck behind it.
    let speed = 40.0_f32;
    let t = ui.ctx().input(|i| i.time) as f32;
    let offset = (t * speed).rem_euclid(cycle);

    painter.galley(
        rect.left_top() - egui::vec2(offset, 0.0),
        galley.clone(),
        color,
    );
    painter.galley(
        rect.left_top() + egui::vec2(cycle - offset, 0.0),
        galley,
        color,
    );

    resp.on_hover_text(text);

    // Keep the frame ticking while the marquee is on screen.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}

/// Build a minimal RedumpMatch from a queue Record when the local DB
/// doesn't have the matching redump entry. The reduced-field match is
/// enough to drive the search query (which only reads title) and the
/// UI's "matched via …" chip; the disc-detail grid will simply show
/// fewer fields.
fn synth_match_from_record(rec: &super::bulk::Record) -> crate::db::RedumpMatch {
    crate::db::RedumpMatch {
        redump_id: rec.redump_id.unwrap_or(0),
        system: rec.system.clone(),
        title: rec.title.clone(),
        foreign_title: None,
        edition: None,
        version: None,
        category: None,
        media: None,
        barcode: None,
        catalog: None,
        pvd_volume_id: None,
        pvd_creation_date: None,
        redump_url: rec.redump_url.clone(),
        matched_via: crate::db::MatchSource::PvdVolumeId,
    }
}

fn filters_eq(a: &super::bulk::QueueFilter, b: &super::bulk::QueueFilter) -> bool {
    a.include_fuzzy == b.include_fuzzy
        && (a.fuzzy_min_score - b.fuzzy_min_score).abs() < 1e-9
}

/// Count how many of `items` are already present in the sidecar `.done.jsonl`
/// next to `job_path`. Used by the bulk loader dialog to show resume info.
fn count_resumed(job_path: &std::path::Path, items: &[super::bulk::QueueItem]) -> usize {
    use std::io::{BufRead, BufReader};
    let mut sidecar = job_path.to_path_buf();
    let name = job_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| format!("{s}.done.jsonl"))
        .unwrap_or_else(|| "queue.done.jsonl".to_string());
    sidecar.set_file_name(name);

    let Ok(file) = std::fs::File::open(&sidecar) else {
        return 0;
    };
    let mut done_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<super::bulk::DoneEntry>(trimmed) {
            done_files.insert(entry.file);
        }
    }
    items.iter().filter(|it| done_files.contains(&it.file)).count()
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

        // Bulk-job loader modal (rendered as a centered Window). Independent
        // of the central-panel ui, so render through the context.
        self.render_bulk_loader(&ctx);

        // Broken-cue prompt — blocks tick_bulk from advancing until the
        // user (or the bulk-mode timeout) resolves it.
        self.render_broken_cue_prompt(&ctx);

        // Bulk-mode keyboard shortcuts.
        self.handle_bulk_hotkeys(&ctx);

        // Drive the bulk queue: load next item if cursor advanced. The
        // broken-cue prompt is a blocker — don't try to load anything else
        // until it resolves.
        if self.broken_cue_prompt.is_none() {
            self.tick_bulk();
        }

        // Top-of-central banner when bulk mode is active.
        self.render_bulk_banner(ui);

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
            // Only treat the disc as a MusicBrainz target when it looks like
            // a pure audio CD: a TOC is present, no data filesystem was
            // detected (no PVD, no HFS/HFS+), AND no redump match was found
            // (redump's PC/Mac catalog is data-only, so a hit there is
            // strong evidence this isn't an audio CD).
            let use_musicbrainz = info_for_search
                .as_ref()
                .map(|i| {
                    i.toc.is_some()
                        && i.pvd.is_none()
                        && i.hfs_mdb.is_none()
                        && i.hfsplus_header.is_none()
                        && i.redump_matches
                            .as_ref()
                            .map(|ms| ms.is_empty())
                            .unwrap_or(true)
                })
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

            // In bulk mode, default-position the window directly below the
            // banner so the controls remain visible. Outside bulk mode, fall
            // through to egui's default placement.
            let default_pos = self.bulk_banner_bottom_y.map(|y| {
                let content = ctx.content_rect();
                egui::pos2(content.left() + 12.0, y + 8.0)
            });
            let mut window = egui::Window::new("Artwork Search")
                .open(&mut self.show_search_window)
                .default_size([default_w, default_h])
                .min_width(600.0)
                .min_height(400.0)
                .resizable(true);
            if let Some(pos) = default_pos {
                window = window.default_pos(pos);
            }
            window.show(&ctx, |ui| {
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

                    ui.horizontal(|ui| {
                        if ui.button("Browse...").clicked() {
                            self.open_file_picker();
                        }
                        if ui.button("Bulk Job...").clicked() {
                            self.open_bulk_job_picker();
                        }
                    });

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
                        let use_musicbrainz = info.toc.is_some()
                            && info.pvd.is_none()
                            && info.hfs_mdb.is_none()
                            && info.hfsplus_header.is_none()
                            && info
                                .redump_matches
                                .as_ref()
                                .map(|ms| ms.is_empty())
                                .unwrap_or(true);
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
                                                ("Catalog", first.catalog.as_deref()),
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

                                // Fuzzy candidates (only when exact match missed)
                                if let Some(fuzzy) = &info.fuzzy_matches {
                                    if !fuzzy.is_empty() {
                                        ui.label("Possible matches:");
                                        ui.vertical(|ui| {
                                            for c in fuzzy {
                                                ui.horizontal(|ui| {
                                                    ui.colored_label(
                                                        egui::Color32::from_rgb(220, 180, 80),
                                                        format!("{:.0}%", c.score * 100.0),
                                                    );
                                                    let mut title = format!(
                                                        "{} (#{})",
                                                        c.title, c.redump_id
                                                    );
                                                    if let Some(v) = &c.inferred_version {
                                                        title.push_str(&format!(" v{v}"));
                                                    }
                                                    ui.label(title);
                                                    ui.hyperlink_to("↗", &c.redump_url);
                                                });
                                                ui.label(
                                                    egui::RichText::new(&c.match_reason)
                                                        .small()
                                                        .color(egui::Color32::GRAY),
                                                );
                                            }
                                        });
                                        ui.end_row();
                                    }
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

                // In-app CD-DA player for CHDs that have audio tracks. Self-
                // guards to a no-op otherwise; placed after the disc_info match
                // so it can take `&mut self` without conflicting with the borrow
                // of `self.disc_info` inside the arms above.
                self.render_audio_player(ui);
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
