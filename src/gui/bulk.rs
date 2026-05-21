//! Bulk-processing queue loaded from `fuzzy_scan --queue` output.
//!
//! Phase 2 scope (this module): types, load-from-disk, resume from sidecar
//! `<job>.done.jsonl`, append entries. The per-item drive (auto-load disc,
//! inject match, auto-search) lives in `app.rs` and uses these types.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use std::cmp::Ordering;

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
    /// Read the job file and a sibling `<job>.done.jsonl` if present. Accepts
    /// either the native queue JSON (a `Vec<QueueItem>`) or the flat-CSV
    /// fuzzy_scan output, which is grouped into QueueItems on the fly.
    /// `reprocess_existing` and `filter` are set at load time and are fixed
    /// for the session.
    pub fn load(
        job_path: PathBuf,
        reprocess_existing: bool,
        filter: &QueueFilter,
    ) -> Result<Self, String> {
        let items = parse_queue_file(&job_path, filter)?;
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

    /// Mark an item by index (used when sibling auto-apply lands on queue
    /// entries other than the active cursor). Caller is responsible for
    /// constructing the entry.
    pub fn record_at(
        &mut self,
        index: usize,
        entry: DoneEntry,
        status: ItemStatus,
    ) -> std::io::Result<()> {
        if let Some(slot) = self.statuses.get_mut(index) {
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

/// Filter knobs applied when parsing/reading a queue source. The bulk loader
/// dialog populates this from its checkboxes/sliders, so the user can broaden
/// or tighten the queue before committing.
#[derive(Clone, Debug)]
pub struct QueueFilter {
    /// When false (default), drop any item whose chosen best record has
    /// `match_type == "fuzzy"`. Exact and musicbrainz tiers are always kept.
    pub include_fuzzy: bool,
    /// When `include_fuzzy` is true, drop fuzzy records below this score.
    /// Ignored when `include_fuzzy` is false.
    pub fuzzy_min_score: f64,
}

impl Default for QueueFilter {
    fn default() -> Self {
        Self {
            include_fuzzy: false,
            fuzzy_min_score: 0.85,
        }
    }
}

/// Read either a native `--queue` JSON file or a flat `fuzzy_scan` CSV
/// (the default output mode), returning the grouped queue. CSV grouping
/// mirrors fuzzy_scan::group_into_queue so both paths produce identical
/// QueueItems for the same set of records.
///
/// `filter` is applied *after* grouping: items whose best record fails the
/// filter (e.g. a low-confidence fuzzy with `include_fuzzy = false`) are
/// dropped. Alternates are also filtered so the user doesn't see junk when
/// flipping to a different candidate.
pub fn parse_queue_file(path: &Path, filter: &QueueFilter) -> Result<Vec<QueueItem>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let is_csv = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    let mut items = if is_csv {
        let records = parse_fuzzy_scan_csv(&raw)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        group_records(records)
    } else if let Ok(items) = serde_json::from_str::<Vec<QueueItem>>(&raw) {
        items
    } else {
        let records: Vec<Record> = serde_json::from_str(&raw)
            .map_err(|e| format!("parse {}: {e}", path.display()))?;
        group_records(records)
    };

    apply_filter(&mut items, filter);
    Ok(items)
}

fn record_passes(r: &Record, filter: &QueueFilter) -> bool {
    match r.match_type.as_str() {
        "exact" | "musicbrainz" => true,
        "fuzzy" => {
            filter.include_fuzzy
                && r.score
                    .map(|s| s >= filter.fuzzy_min_score)
                    .unwrap_or(false)
        }
        _ => false,
    }
}

fn apply_filter(items: &mut Vec<QueueItem>, filter: &QueueFilter) {
    items.retain_mut(|it| {
        it.alternates.retain(|r| record_passes(r, filter));
        if record_passes(&it.best, filter) {
            return true;
        }
        // The best failed the filter — promote the strongest surviving
        // alternate (if any) so the item stays in the queue.
        if let Some(promoted) = it.alternates.first().cloned() {
            it.best = promoted;
            it.alternates.remove(0);
            true
        } else {
            false
        }
    });
}

fn rank_record(r: &Record) -> u8 {
    match r.match_type.as_str() {
        "exact" => 0,
        "musicbrainz" => 1,
        "fuzzy" => 2,
        _ => 3,
    }
}

/// Same grouping/ranking as fuzzy_scan's --queue writer: order preserved by
/// first appearance of each `file`, intra-file rows sorted exact > MB >
/// fuzzy-by-score-desc, files with no usable records dropped.
fn group_records(records: Vec<Record>) -> Vec<QueueItem> {
    let mut groups: Vec<(String, Vec<Record>)> = Vec::new();
    for r in records {
        if let Some(last) = groups.last_mut() {
            if last.0 == r.file {
                last.1.push(r);
                continue;
            }
        }
        groups.push((r.file.clone(), vec![r]));
    }

    let mut items = Vec::new();
    for (file, mut recs) in groups {
        recs.retain(|r| r.status == "ok" && r.match_type != "none");
        if recs.is_empty() {
            continue;
        }
        recs.sort_by(|a, b| {
            rank_record(a).cmp(&rank_record(b)).then_with(|| {
                b.score
                    .unwrap_or(0.0)
                    .partial_cmp(&a.score.unwrap_or(0.0))
                    .unwrap_or(Ordering::Equal)
            })
        });
        let has_existing_art = has_sidecar_art(Path::new(&file));
        let best = recs.remove(0);
        items.push(QueueItem {
            file,
            best,
            alternates: recs,
            has_existing_art,
        });
    }
    items
}

fn has_sidecar_art(disc_path: &Path) -> bool {
    let Some(stem) = disc_path.file_stem() else { return false; };
    let Some(dir) = disc_path.parent() else { return false; };
    for ext in ["jpg", "jpeg", "png"] {
        let candidate = dir.join(format!("{}.{}", stem.to_string_lossy(), ext));
        if candidate.is_file() {
            return true;
        }
    }
    false
}

/// Minimal CSV reader for fuzzy_scan's exact 12-column output. Handles
/// "double-quoted, embedded-quote-escape-via-"", and "" cells. We don't pull
/// in a CSV crate because the format we're consuming is fully controlled by
/// fuzzy_scan::write_csv next door.
///
/// Column order (must match write_csv):
///   file,status,match_type,redump_id,title,system,score,sources,
///   inferred_version,size_ratio,reason,redump_url
fn parse_fuzzy_scan_csv(raw: &str) -> Result<Vec<Record>, String> {
    let rows = parse_csv_rows(raw)?;
    let mut iter = rows.into_iter();
    let header = iter
        .next()
        .ok_or_else(|| "csv is empty (no header row)".to_string())?;
    if header.first().map(|s| s.as_str()) != Some("file") {
        return Err(format!(
            "csv header doesn't look like fuzzy_scan output (first column = {:?})",
            header.first()
        ));
    }
    let expected = [
        "file",
        "status",
        "match_type",
        "redump_id",
        "title",
        "system",
        "score",
        "sources",
        "inferred_version",
        "size_ratio",
        "reason",
        "redump_url",
    ];
    if header.len() != expected.len() {
        return Err(format!(
            "csv has {} columns, expected {}",
            header.len(),
            expected.len()
        ));
    }
    for (i, (got, want)) in header.iter().zip(expected.iter()).enumerate() {
        if got != want {
            return Err(format!(
                "csv column {i} is {got:?}, expected {want:?}"
            ));
        }
    }

    let mut out = Vec::new();
    for (line_no, row) in iter.enumerate() {
        if row.iter().all(|c| c.is_empty()) {
            continue;
        }
        if row.len() != expected.len() {
            return Err(format!(
                "csv row {} has {} columns, expected {}",
                line_no + 2,
                row.len(),
                expected.len()
            ));
        }
        let parse_opt_i64 = |s: &str| -> Result<Option<i64>, String> {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<i64>()
                    .map(Some)
                    .map_err(|e| format!("redump_id parse: {e}"))
            }
        };
        let parse_opt_f64 = |s: &str| -> Result<Option<f64>, String> {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<f64>()
                    .map(Some)
                    .map_err(|e| format!("float parse: {e}"))
            }
        };
        out.push(Record {
            file: row[0].clone(),
            status: row[1].clone(),
            match_type: row[2].clone(),
            redump_id: parse_opt_i64(&row[3])?,
            title: row[4].clone(),
            system: row[5].clone(),
            score: parse_opt_f64(&row[6])?,
            sources: row[7].clone(),
            inferred_version: row[8].clone(),
            size_ratio: parse_opt_f64(&row[9])?,
            reason: row[10].clone(),
            redump_url: row[11].clone(),
        });
    }
    Ok(out)
}

/// Split a CSV blob into rows of cells. Honors RFC 4180-style double-quoted
/// fields with `""` as an embedded quote and CR/LF inside quoted cells.
fn parse_csv_rows(raw: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut in_quotes = false;
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    cell.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
            } else {
                cell.push(b as char);
                i += 1;
            }
        } else {
            match b {
                b'"' => {
                    in_quotes = true;
                    i += 1;
                }
                b',' => {
                    row.push(std::mem::take(&mut cell));
                    i += 1;
                }
                b'\n' => {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                    i += 1;
                }
                b'\r' => {
                    // Treat \r\n as one separator; bare \r as a row break.
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                    i += 1;
                    if i < bytes.len() && bytes[i] == b'\n' {
                        i += 1;
                    }
                }
                _ => {
                    cell.push(b as char);
                    i += 1;
                }
            }
        }
    }
    if in_quotes {
        return Err("csv ended inside a quoted field".to_string());
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    Ok(rows)
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
