//! Bulk-processing queue loaded from `fuzzy_scan --queue` output.
//!
//! Phase 2 scope (this module): types, load-from-disk, resume from sidecar
//! `<job>.done.jsonl`, append entries. The per-item drive (auto-load disc,
//! inject match, auto-search) lives in `app.rs` and uses these types.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Mirrors `fuzzy_scan::Record`. Kept independent so the GUI doesn't depend on
/// the binary crate; structurally identical so the JSON round-trips.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
    pub file: String,
    pub status: String,
    pub match_type: String,
    pub redump_id: Option<i64>,
    pub title: String,
    pub system: String,
    pub score: Option<f64>,
    pub sources: String,
    pub inferred_version: String,
    pub size_ratio: Option<f64>,
    pub reason: String,
    pub redump_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueueItem {
    pub file: String,
    pub best: Record,
    pub alternates: Vec<Record>,
    pub has_existing_art: bool,
}

/// In-memory per-item state. Persisted state lives in the sidecar log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    /// Not yet processed in this or any prior session.
    Pending,
    /// User saved artwork.
    Saved,
    /// User explicitly skipped.
    Skipped,
    /// Auto-skipped because a sidecar JPG was already present.
    ExistingArt,
}

/// One row appended to the sidecar log when an item resolves. The schema is
/// deliberately small and flat so it's grep-able and easy to audit later.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DoneEntry {
    pub file: String,
    /// "saved" | "skipped" | "existing-art"
    pub status: String,
    pub queue_redump_id: Option<i64>,
    /// Set only when the background hash cascade returned a redump_id that
    /// disagreed with the queue's pre-populated match. See the bulk-mode plan.
    pub hash_redump_id: Option<i64>,
    pub image_url: Option<String>,
    /// ISO-8601 UTC timestamp.
    pub ts: String,
}

pub struct BulkQueue {
    pub job_path: PathBuf,
    pub items: Vec<QueueItem>,
    pub statuses: Vec<ItemStatus>,
    pub cursor: usize,
    pub reprocess_existing: bool,
    done_log_path: PathBuf,
    done_log: Option<BufWriter<File>>,
}

impl BulkQueue {
    /// Read the job JSON and a sibling `<job>.done.jsonl` if present.
    /// `reprocess_existing` is set at load time and is fixed for the session.
    pub fn load(job_path: PathBuf, reprocess_existing: bool) -> Result<Self, String> {
        let raw = std::fs::read_to_string(&job_path)
            .map_err(|e| format!("read {}: {e}", job_path.display()))?;
        let items: Vec<QueueItem> = serde_json::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", job_path.display()))?;
        if items.is_empty() {
            return Err("queue file contains zero items".to_string());
        }

        let mut statuses = vec![ItemStatus::Pending; items.len()];
        let done_log_path = sidecar_done_path(&job_path);

        // Replay the sidecar log so resuming a job skips work already done.
        // The log is append-only, latest entry wins.
        if let Ok(file) = File::open(&done_log_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(entry): Result<DoneEntry, _> = serde_json::from_str(&line) else {
                    continue;
                };
                let status = match entry.status.as_str() {
                    "saved" => ItemStatus::Saved,
                    "skipped" => ItemStatus::Skipped,
                    "existing-art" => ItemStatus::ExistingArt,
                    _ => continue,
                };
                if let Some(idx) = items.iter().position(|it| it.file == entry.file) {
                    statuses[idx] = status;
                }
            }
        }

        // Start cursor at the first non-Pending item if we resumed; otherwise 0.
        let cursor = statuses
            .iter()
            .position(|s| *s == ItemStatus::Pending)
            .unwrap_or(0);

        Ok(Self {
            job_path,
            items,
            statuses,
            cursor,
            reprocess_existing,
            done_log_path,
            done_log: None,
        })
    }

    pub fn total(&self) -> usize {
        self.items.len()
    }

    /// How many items finished in any state (saved/skipped/existing-art).
    pub fn finished_count(&self) -> usize {
        self.statuses
            .iter()
            .filter(|s| **s != ItemStatus::Pending)
            .count()
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.items.get(self.cursor)
    }

    pub fn current_status(&self) -> Option<ItemStatus> {
        self.statuses.get(self.cursor).copied()
    }

    /// Advance cursor to the next Pending item. Wraps to end (saturating) when
    /// there's nothing left to do; callers detect completion via
    /// `is_complete()`.
    pub fn advance(&mut self) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        for step in 1..=n {
            let idx = (self.cursor + step) % n;
            if self.statuses[idx] == ItemStatus::Pending {
                self.cursor = idx;
                return;
            }
        }
        // No pending items left — park cursor at end so `current()` returns
        // the final processed item rather than something arbitrary.
        self.cursor = n - 1;
    }

    /// Move backward to the previous item (any status). Useful for revisiting
    /// a wrong save.
    pub fn back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.statuses.iter().all(|s| *s != ItemStatus::Pending)
    }

    /// Mark the current item with a status and append a sidecar log entry.
    pub fn record(&mut self, entry: DoneEntry, status: ItemStatus) -> std::io::Result<()> {
        if let Some(slot) = self.statuses.get_mut(self.cursor) {
            *slot = status;
        }
        self.append_done(&entry)
    }

    fn append_done(&mut self, entry: &DoneEntry) -> std::io::Result<()> {
        if self.done_log.is_none() {
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.done_log_path)?;
            self.done_log = Some(BufWriter::new(f));
        }
        let line = serde_json::to_string(entry).expect("DoneEntry serializes");
        if let Some(w) = self.done_log.as_mut() {
            writeln!(w, "{line}")?;
            w.flush()?;
        }
        Ok(())
    }
}

fn sidecar_done_path(job_path: &Path) -> PathBuf {
    let mut p = job_path.to_path_buf();
    let new_name = match job_path.file_name().and_then(|n| n.to_str()) {
        Some(name) => format!("{name}.done.jsonl"),
        None => "queue.done.jsonl".to_string(),
    };
    p.set_file_name(new_name);
    p
}

/// Format current UTC time as an ISO-8601 string for the sidecar log.
/// Uses Howard Hinnant's days-from-civil inverse so we don't pull in chrono
/// for a single line of output.
pub fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = unix_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn unix_to_ymdhms(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86_400);
    let secs_of_day = t.rem_euclid(86_400) as u32;
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;

    let z = days + 719468;
    let era = if z >= 0 { z / 146097 } else { (z - 146096) / 146097 };
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y_base = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y_base + 1 } else { y_base };
    (y, m, d, h, mi, s)
}
