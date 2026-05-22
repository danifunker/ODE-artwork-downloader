//! Fuzzy redump matching.
//!
//! Runs after the exact-match cascade in `lookup.rs` misses. Returns a ranked
//! list of *possible* matches with confidence scores rather than a single
//! answer — the caller decides what to do with the list. See
//! `docs/fuzzy_match_against_redump.md` for the design.
//!
//! Four scorers contribute:
//!   - Source A — relaxed PVD match (volume label + creation date)
//!   - Source B — title fuzzy (token-set / acronym / substring)
//!   - Source C — track-signature match (count ±1, per-track duration)
//!   - Source D — payload-size sanity (modifier, drops or penalizes)
//!
//! Thresholds come from `FuzzyMatchConfig` and are intentionally loose for the
//! initial data-collection phase.

use std::collections::HashMap;

use rusqlite::Connection;
use strsim::normalized_levenshtein;

use crate::config::FuzzyMatchConfig;

/// Which scorer produced (or agreed on) a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreSource {
    Pvd,
    Title,
    Tracks,
}

/// One ranked fuzzy candidate. Never a committed match — just a possibility.
#[derive(Debug, Clone)]
pub struct FuzzyCandidate {
    pub redump_id: i64,
    pub system: String,
    pub title: String,
    pub redump_url: String,
    pub score: f64,
    pub sources: Vec<ScoreSource>,
    /// Payload-size ratio vs. the disc (from Source D), when computable.
    pub size_ratio: Option<f64>,
    /// Version parsed off a `TITLE_NNN`-style volume label, e.g. `1.06`.
    pub inferred_version: Option<String>,
    /// PVD creation date (ISO-8601 string as stored by redump). Used by the
    /// verification pass for date-based corroboration.
    pub pvd_creation_date: Option<String>,
    pub match_reason: String,
}

/// Everything the fuzzy matcher can use, pre-extracted from the disc so this
/// module stays free of disc-reading concerns.
#[derive(Debug, Default, Clone)]
pub struct FuzzyInputs {
    /// Candidate title strings pulled from the disc, in priority order
    /// (volume label, parsed filename, on-disc hints).
    pub title_candidates: Vec<String>,
    /// Raw PVD volume label (pre-normalization), if any.
    pub pvd_volume_id: Option<String>,
    /// PVD creation date (ISO-8601 string), if any.
    pub pvd_creation_date: Option<String>,
    /// Per-track durations in CD frames, in track order. Empty if unknown.
    pub disc_track_frames: Vec<u32>,
    /// Total data payload size in bytes, if computable.
    pub disc_payload_bytes: Option<u64>,
}

// ── Scoring primitives (pure, unit-tested) ──────────────────────────────────

/// Lowercase, turn separators into spaces, collapse whitespace.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strip a trailing version suffix from a volume label and return
/// `(stem, version)`. `QUAKE_106` → (`QUAKE`, Some("1.06")), `DOOM2_19` →
/// (`DOOM2`, Some("1.9")). Returns the input unchanged with `None` when there
/// is no recognizable suffix.
pub fn strip_version_suffix(label: &str) -> (String, Option<String>) {
    // Match a trailing `_<digits>` or `_V<digits>` / `-V<digits>` group.
    if let Some(idx) = label.rfind(['_', '-']) {
        let (stem, sep_and_rest) = label.split_at(idx);
        let rest = &sep_and_rest[1..];
        let digits: &str = rest.strip_prefix(['V', 'v']).unwrap_or(rest);
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) && !stem.is_empty() {
            let version = format_version(digits);
            return (stem.to_string(), Some(version));
        }
    }
    (label.to_string(), None)
}

/// Generate variant forms of a candidate string so disc labels like
/// `QUAKE106` / `MortalKombat3` / `QUAKE_106` reach the right redump title.
/// Returns the original plus any non-trivial transformations, deduped.
///
/// - **Letter↔digit split** turns mashed-together tokens into separated ones
///   (`QUAKE106` → `quake 106`, `TR1` → `tr 1`). Safe: pure-letter and
///   pure-digit strings are unchanged.
/// - **CamelCase split** separates capital-letter run boundaries
///   (`MortalKombat3` → `Mortal Kombat 3`, `GLQuake` → `GL Quake`).
/// - **Version-suffix strip** drops trailing `_NN` / `_VN` patch markers
///   (`QUAKE_106` → `QUAKE` — patched version of the same game).
///
/// We keep the original because Discworld-shaped names must stay intact
/// (`Discworld` → `Discworld`, since case-aware split happens only at the
/// **first** boundary; this implementation only splits real boundaries
/// reflected in the input character classes).
pub fn label_variants(s: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![s.to_string()];
    let push = |out: &mut Vec<String>, v: String| {
        if !v.trim().is_empty() && !out.iter().any(|e| e.eq_ignore_ascii_case(&v)) {
            out.push(v);
        }
    };
    let ld = crate::disc::content::letter_digit_split(s);
    push(&mut out, ld.clone());
    let cc = crate::disc::content::camel_split(s);
    push(&mut out, cc.clone());
    // Chained: camel-split THEN letter-digit-split, so `MortalKombat3` becomes
    // `Mortal Kombat 3` (camel handles Kombat boundary, digit handles the 3).
    let chained = crate::disc::content::letter_digit_split(&cc);
    push(&mut out, chained);
    let (stem, _) = strip_version_suffix(s);
    push(&mut out, stem);
    out
}

/// `106` → `1.06`, `19` → `1.9`, `200` → `2.00`. A bare single digit stays
/// as-is (`5` → `5`).
fn format_version(digits: &str) -> String {
    if digits.len() <= 1 {
        return digits.to_string();
    }
    let (major, minor) = digits.split_at(1);
    format!("{major}.{minor}")
}

/// Roman numeral → arabic, for tokens like `II`, `IV`, `VIII`. Returns `None`
/// if the token is not a (supported) roman numeral.
pub fn roman_to_arabic(token: &str) -> Option<u32> {
    let t = token.to_ascii_uppercase();
    let val = |c: char| match c {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        _ => None,
    };
    if t.is_empty() || !t.chars().all(|c| val(c).is_some()) {
        return None;
    }
    let nums: Vec<u32> = t.chars().map(|c| val(c).unwrap()).collect();
    let mut total = 0i64;
    for i in 0..nums.len() {
        if i + 1 < nums.len() && nums[i] < nums[i + 1] {
            total -= nums[i] as i64;
        } else {
            total += nums[i] as i64;
        }
    }
    if (1..=39).contains(&total) {
        Some(total as u32)
    } else {
        None
    }
}

/// Generic disc-vocabulary tokens that carry no title identity. Stripped
/// from token sets before scoring so a volume label like `CANADA_DISC`
/// can't ride a shared "disc" word to a 100% subset reward against an
/// unrelated candidate title that happens to mention "disc". Match is
/// against the lowercased, alphanumeric-only token — so "Discworld" stays
/// intact (single token), but `cd-rom` splits to `cd`+`rom` and both fall.
fn is_generic_disc_token(t: &str) -> bool {
    matches!(
        t,
        "disc" | "disk" | "discs" | "disks"
            | "cd" | "cds" | "rom" | "roms" | "cdrom" | "cdroms"
            | "dvd" | "dvds" | "dvdrom" | "dvdroms"
            | "iso" | "img" | "image"
            | "volume" | "vol"
            | "game" | "games"
    )
}

/// Tokenize: normalize, expand roman numerals, drop generic disc-vocabulary
/// words, return the sorted unique token set.
fn token_set(s: &str) -> Vec<String> {
    let mut toks: Vec<String> = normalize(s)
        .split_whitespace()
        .map(|t| {
            roman_to_arabic(t)
                .map(|n| n.to_string())
                .unwrap_or_else(|| t.to_string())
        })
        .filter(|t| !is_generic_disc_token(t))
        .collect();
    toks.sort();
    toks.dedup();
    toks
}

/// Token-set ratio (à la fuzzywuzzy): compares the sorted intersection of the
/// two token sets against each full set. Rewards the case where one string's
/// tokens are a subset of the other's — so a disc title matches a redump title
/// that merely adds a subtitle. Tolerant of word reordering too.
pub fn token_set_ratio(a: &str, b: &str) -> f64 {
    let sa = token_set(a);
    let sb = token_set(b);
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let inter: Vec<&String> = sa.iter().filter(|t| sb.contains(t)).collect();
    let join = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");

    let inter_str = join(&inter);
    let diff_a: Vec<&String> = sa.iter().filter(|t| !sb.contains(t)).collect();
    let diff_b: Vec<&String> = sb.iter().filter(|t| !sa.contains(t)).collect();
    let combined_a = if diff_a.is_empty() {
        inter_str.clone()
    } else {
        format!("{} {}", inter_str, join(&diff_a))
    };
    let combined_b = if diff_b.is_empty() {
        inter_str.clone()
    } else {
        format!("{} {}", inter_str, join(&diff_b))
    };

    // r3 compares the two full token sets — always safe.
    let r3 = normalized_levenshtein(&combined_a, &combined_b);

    // r1/r2 reward the case where the intersection alone matches one full set
    // (i.e. one string's tokens are a subset of the other). That's powerful for
    // subtitle matches ("Space Quest 6" ⊆ "Space Quest 6: ...") but dangerous
    // for a single shared common word ("Rescue" ⊆ "Barbie: Pet Rescue"). Only
    // grant the subset reward when at least TWO tokens are shared; a 1-token
    // overlap must stand on r3, which penalizes the length difference.
    if inter.len() >= 2 {
        let r1 = normalized_levenshtein(&inter_str, &combined_a);
        let r2 = normalized_levenshtein(&inter_str, &combined_b);
        r1.max(r2).max(r3)
    } else {
        r3
    }
}

/// Build an acronym from a title's significant words: first letter of each
/// word, with roman numerals / standalone digits expanded to arabic. `Super
/// Street Fighter II Turbo` → `ssf2t`, `Space Quest V` → `sq5`.
pub fn build_acronym(title: &str) -> String {
    let mut out = String::new();
    for word in normalize(title).split_whitespace() {
        if let Some(n) = roman_to_arabic(word) {
            out.push_str(&n.to_string());
        } else if word.chars().all(|c| c.is_ascii_digit()) {
            out.push_str(word);
        } else if let Some(first) = word.chars().next() {
            out.push(first);
        }
    }
    out
}

/// Acronym variants for a redump title: the full title and, when the title
/// has a subtitle (split on `:` or ` - `), the leading segment too. Lets `sq6`
/// match `Space Quest 6: Roger Wilco...` via the main-title acronym.
pub fn title_acronyms(title: &str) -> Vec<String> {
    let mut out = vec![build_acronym(title)];
    let head = title
        .split(':')
        .next()
        .and_then(|s| s.split(" - ").next())
        .unwrap_or(title);
    let head_ac = build_acronym(head);
    if !head_ac.is_empty() && !out.contains(&head_ac) {
        out.push(head_ac);
    }
    out.retain(|a| !a.is_empty());
    out
}

/// Substring-containment score: if one normalized string contains the other
/// (≥3 chars), score is `len(shorter)/len(longer)`, else 0.
pub fn substring_containment(a: &str, b: &str) -> f64 {
    let na = normalize(a).replace(' ', "");
    let nb = normalize(b).replace(' ', "");
    if na.len() < 3 || nb.len() < 3 {
        return 0.0;
    }
    let (short, long) = if na.len() <= nb.len() {
        (&na, &nb)
    } else {
        (&nb, &na)
    };
    if long.contains(short.as_str()) {
        short.len() as f64 / long.len() as f64
    } else {
        0.0
    }
}

/// Best title-matcher score for one disc-side candidate against one redump
/// title, plus the name of the winning matcher.
fn best_title_score(candidate: &str, title: &str) -> (f64, &'static str) {
    let ts = token_set_ratio(candidate, title);
    let mut best = (ts, "token-set");

    // Acronym only when the candidate plausibly *is* an acronym: short,
    // single-token, no spaces.
    let norm_cand = normalize(candidate);
    if !norm_cand.is_empty() && norm_cand.len() <= 8 && !norm_cand.contains(' ') {
        if title_acronyms(title).iter().any(|a| *a == norm_cand) {
            return (1.0, "acronym");
        }
    }

    let sub = substring_containment(candidate, title);
    if sub > best.0 {
        best = (sub, "substring");
    }
    best
}

// ── DB helpers ──────────────────────────────────────────────────────────────

struct DiscRow {
    redump_id: i64,
    system: String,
    title: String,
    redump_url: String,
    pvd_creation_date: Option<String>,
}

fn select_disc_rows(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> rusqlite::Result<Vec<DiscRow>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, |row| {
        Ok(DiscRow {
            redump_id: row.get(0)?,
            system: row.get(1)?,
            title: row.get(2)?,
            redump_url: row.get(3)?,
            pvd_creation_date: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// All disc rows. Used by the acronym pass, which can't be FTS-prefiltered.
/// ~60k rows; loaded only when an acronym-shaped candidate is present.
fn all_disc_rows(conn: &Connection) -> rusqlite::Result<Vec<DiscRow>> {
    select_disc_rows(
        conn,
        "SELECT redump_id, system, title, redump_url, pvd_creation_date FROM redump_disc",
        &[],
    )
}

/// FTS prefilter for a normalized query string. Tokens are OR'd; the query is
/// sanitized to bare alphanumeric tokens so user/disc text can't break FTS5
/// syntax.
fn fts_candidates(conn: &Connection, query: &str, limit: i64) -> rusqlite::Result<Vec<DiscRow>> {
    let tokens: Vec<String> = normalize(query)
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let fts_query = tokens.join(" OR ");
    let sql = "SELECT d.redump_id, d.system, d.title, d.redump_url, d.pvd_creation_date \
               FROM redump_disc_fts JOIN redump_disc d ON d.redump_id = redump_disc_fts.rowid \
               WHERE redump_disc_fts MATCH ?1 ORDER BY bm25(redump_disc_fts) LIMIT ?2";
    select_disc_rows(conn, sql, &[&fts_query, &limit])
}

/// Per-disc track summary used by Sources C and D.
struct TrackSummary {
    /// Per-track durations in CD frames, in track order.
    frames: Vec<u32>,
    /// Total payload bytes across all tracks.
    total_bytes: u64,
}

/// Fallback when `tracks.sectors` is NULL (pre-v2 rows). Redump `size_bytes`
/// is cooked (2048 for Mode-1 data, 2352 for audio), so we divide by the
/// per-sector payload to approximate the frame count.
fn bytes_to_frames(kind: &str, size_bytes: u64) -> u32 {
    let per = if kind.eq_ignore_ascii_case("audio") { 2352 } else { 2048 };
    (size_bytes / per) as u32
}

fn track_summary(conn: &Connection, redump_id: i64) -> rusqlite::Result<TrackSummary> {
    let mut stmt = conn.prepare(
        "SELECT kind, sectors, size_bytes FROM redump_track WHERE redump_id = ?1 ORDER BY number",
    )?;
    let rows = stmt.query_map([redump_id], |row| {
        let kind: Option<String> = row.get(0)?;
        let sectors: Option<i64> = row.get(1)?;
        let size: Option<i64> = row.get(2)?;
        Ok((
            kind.unwrap_or_default(),
            sectors.map(|s| s.max(0) as u32),
            size.unwrap_or(0).max(0) as u64,
        ))
    })?;
    let mut frames = Vec::new();
    let mut total_bytes = 0u64;
    for r in rows {
        let (kind, sectors, size) = r?;
        total_bytes += size;
        frames.push(sectors.unwrap_or_else(|| bytes_to_frames(&kind, size)));
    }
    Ok(TrackSummary { frames, total_bytes })
}

// ── Scratch candidate accumulated across sources before merge ────────────────

struct Scratch {
    row: DiscRow,
    score: f64,
    sources: Vec<ScoreSource>,
    inferred_version: Option<String>,
    reason: String,
}

fn upsert(
    acc: &mut HashMap<i64, Scratch>,
    row: DiscRow,
    score: f64,
    source: ScoreSource,
    reason: String,
    inferred_version: Option<String>,
) {
    let entry = acc.entry(row.redump_id).or_insert_with(|| Scratch {
        row: DiscRow {
            redump_id: row.redump_id,
            system: row.system.clone(),
            title: row.title.clone(),
            redump_url: row.redump_url.clone(),
            pvd_creation_date: row.pvd_creation_date.clone(),
        },
        score: 0.0,
        sources: Vec::new(),
        inferred_version: None,
        reason: String::new(),
    });
    if !entry.sources.contains(&source) {
        entry.sources.push(source);
    }
    if score > entry.score {
        entry.score = score;
        entry.reason = reason;
    }
    if entry.inferred_version.is_none() {
        if let Some(v) = inferred_version {
            entry.inferred_version = Some(v);
        }
    }
}

// ── Sources ──────────────────────────────────────────────────────────────────

/// Source B — title fuzzy. The workhorse; runs against fully-populated titles.
fn source_title(
    conn: &Connection,
    inputs: &FuzzyInputs,
    cfg: &FuzzyMatchConfig,
    acc: &mut HashMap<i64, Scratch>,
) -> rusqlite::Result<()> {
    // Lazily loaded full row set for the acronym pass.
    let mut all_rows: Option<Vec<DiscRow>> = None;

    // Expand each candidate with a version-suffix-stripped variant so
    // volume-label-derived titles like `QUAKE_106` match `Quake`.
    let mut expanded: Vec<String> = Vec::new();
    for cand in &inputs.title_candidates {
        let t = cand.trim();
        if t.is_empty() {
            continue;
        }
        if !expanded.iter().any(|e| e.eq_ignore_ascii_case(t)) {
            expanded.push(t.to_string());
        }
        let (stem, ver) = strip_version_suffix(t);
        if ver.is_some() && !expanded.iter().any(|e| e.eq_ignore_ascii_case(&stem)) {
            expanded.push(stem);
        }
    }

    for cand in &expanded {
        let cand = cand.as_str();

        // FTS-prefiltered token-set / substring scoring.
        for row in fts_candidates(conn, cand, (cfg.candidate_cap as i64) * 4)? {
            let (score, matcher) = best_title_score(cand, &row.title);
            if score >= cfg.source_threshold {
                let reason = format!("title {matcher} {score:.2} vs {:?}", cand);
                upsert(acc, row, score, ScoreSource::Title, reason, None);
            }
        }

        // Acronym pass — only for acronym-shaped candidates, and only then do
        // we pay for the full-table scan.
        let norm = normalize(cand);
        if !norm.is_empty() && norm.len() <= 8 && !norm.contains(' ') {
            let rows = match &all_rows {
                Some(r) => r,
                None => {
                    all_rows = Some(all_disc_rows(conn)?);
                    all_rows.as_ref().unwrap()
                }
            };
            for row in rows {
                if title_acronyms(&row.title).iter().any(|a| *a == norm) {
                    let clone = DiscRow {
                        redump_id: row.redump_id,
                        system: row.system.clone(),
                        title: row.title.clone(),
                        redump_url: row.redump_url.clone(),
                        pvd_creation_date: row.pvd_creation_date.clone(),
                    };
                    let reason = format!("title acronym {norm} = {:?}", row.title);
                    upsert(acc, clone, 1.0, ScoreSource::Title, reason, None);
                }
            }
        }
    }
    Ok(())
}

/// Source A — relaxed PVD match. Dormant until the re-seed populates
/// `pvd_volume_id`; the volume branch simply finds no rows until then.
fn source_pvd(
    conn: &Connection,
    inputs: &FuzzyInputs,
    cfg: &FuzzyMatchConfig,
    acc: &mut HashMap<i64, Scratch>,
) -> rusqlite::Result<()> {
    let Some(raw_vol) = inputs.pvd_volume_id.as_deref().filter(|s| !s.trim().is_empty()) else {
        return Ok(());
    };
    let (disc_stem, _disc_ver) = strip_version_suffix(raw_vol.trim());

    // Candidate pool: every disc that has a non-empty volume label. Currently
    // ~0 rows; bounded and cheap. Revisit if the re-seed makes this large.
    let rows = select_disc_rows(
        conn,
        "SELECT redump_id, system, title, redump_url, pvd_creation_date FROM redump_disc \
         WHERE pvd_volume_id IS NOT NULL AND pvd_volume_id <> ''",
        &[],
    )?;
    // We also need each candidate's own volume label to compare against.
    let mut stmt = conn.prepare(
        "SELECT pvd_volume_id FROM redump_disc WHERE redump_id = ?1",
    )?;

    for row in rows {
        let cand_vol: Option<String> =
            stmt.query_row([row.redump_id], |r| r.get(0)).ok();
        let Some(cand_vol) = cand_vol else { continue };
        let (cand_stem, cand_ver) = strip_version_suffix(cand_vol.trim());
        let sim = normalized_levenshtein(
            &normalize(&disc_stem),
            &normalize(&cand_stem),
        );
        if sim >= cfg.source_threshold {
            let reason = format!("pvd volume {sim:.2}: {:?} ~ {:?}", raw_vol, cand_vol);
            upsert(acc, row, sim, ScoreSource::Pvd, reason, cand_ver);
        }
    }
    Ok(())
}

/// Source C — track-signature match. Aligns disc-side per-track frame
/// durations against each candidate's track list, tolerating one extra/missing
/// track. NOTE: `DiscTOC` does not retain per-track type, so data/audio
/// alignment is not enforced here — matching is on count + per-track duration
/// only. Tighten once disc-side track types are available.
fn source_tracks(
    conn: &Connection,
    inputs: &FuzzyInputs,
    cfg: &FuzzyMatchConfig,
    acc: &mut HashMap<i64, Scratch>,
) -> rusqlite::Result<()> {
    let disc = &inputs.disc_track_frames;
    // Low track counts (1–2) are too generic — a lone data track matches any
    // single-track redump entry of similar length. Require a minimum.
    if disc.len() < cfg.min_tracks_for_signature {
        return Ok(());
    }
    let n = disc.len() as i64;

    // Only consider discs whose track count is within ±1.
    let rows = select_disc_rows(
        conn,
        "SELECT d.redump_id, d.system, d.title, d.redump_url, d.pvd_creation_date FROM redump_disc d \
         WHERE (SELECT COUNT(*) FROM redump_track t WHERE t.redump_id = d.redump_id) \
               BETWEEN ?1 AND ?2",
        &[&(n - 1), &(n + 1)],
    )?;

    for row in rows {
        let summary = track_summary(conn, row.redump_id)?;
        let score = align_tracks(disc, &summary.frames, cfg.track_frame_tolerance);
        if score >= cfg.source_threshold {
            let reason = format!(
                "tracks {score:.2} ({} disc / {} redump)",
                disc.len(),
                summary.frames.len()
            );
            upsert(acc, row, score, ScoreSource::Tracks, reason, None);
        }
    }
    Ok(())
}

/// Greedy left-to-right alignment allowing a single insertion/deletion.
/// Score = matched / max(len_a, len_b). A track matches when its frame
/// duration is within `tol`.
fn align_tracks(a: &[u32], b: &[u32], tol: u32) -> f64 {
    let close = |x: u32, y: u32| x.abs_diff(y) <= tol;
    let (mut i, mut j, mut matched) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        if close(a[i], b[j]) {
            matched += 1;
            i += 1;
            j += 1;
        } else if a.len() > b.len() {
            i += 1; // skip an extra disc track
        } else if b.len() > a.len() {
            j += 1; // skip an extra redump track
        } else {
            i += 1;
            j += 1; // equal length, mismatch — advance both
        }
    }
    let denom = a.len().max(b.len());
    if denom == 0 {
        0.0
    } else {
        matched as f64 / denom as f64
    }
}

/// Source D — payload-size sanity. Modifies (penalizes/drops) candidates;
/// never generates them. Returns the size ratio for display.
fn apply_size_sanity(
    conn: &Connection,
    inputs: &FuzzyInputs,
    cfg: &FuzzyMatchConfig,
    scratch: &Scratch,
) -> rusqlite::Result<Option<(f64, f64)>> {
    // (multiplier, ratio). multiplier 0.0 means "drop".
    let Some(disc_bytes) = inputs.disc_payload_bytes.filter(|b| *b > 0) else {
        return Ok(None);
    };
    let summary = track_summary(conn, scratch.row.redump_id)?;
    if summary.total_bytes == 0 {
        return Ok(None);
    }
    let (lo, hi) = (
        disc_bytes.min(summary.total_bytes) as f64,
        disc_bytes.max(summary.total_bytes) as f64,
    );
    let ratio = lo / hi;
    let mult = if ratio >= cfg.size_ok_ratio {
        1.0
    } else if ratio >= cfg.size_drop_ratio {
        cfg.size_penalty
    } else {
        0.0
    };
    Ok(Some((mult, ratio)))
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run all fuzzy sources and return ranked candidates above the merged floor,
/// capped at `candidate_cap`. Empty when nothing clears the floor.
pub fn fuzzy_search(
    conn: &Connection,
    inputs: &FuzzyInputs,
    cfg: &FuzzyMatchConfig,
) -> rusqlite::Result<Vec<FuzzyCandidate>> {
    let mut acc: HashMap<i64, Scratch> = HashMap::new();

    source_title(conn, inputs, cfg, &mut acc)?;
    source_pvd(conn, inputs, cfg, &mut acc)?;
    source_tracks(conn, inputs, cfg, &mut acc)?;

    let mut out: Vec<FuzzyCandidate> = Vec::with_capacity(acc.len());
    for (_, scratch) in acc.into_iter().collect::<Vec<_>>() {
        // Agreement bonus: +bonus per source beyond the first, capped at 2x.
        let extra = scratch.sources.len().saturating_sub(1).min(2);
        let mut score = (scratch.score + cfg.agreement_bonus * extra as f64).min(1.0);

        // Source D modifier.
        let mut size_ratio = None;
        if let Some((mult, ratio)) = apply_size_sanity(conn, inputs, cfg, &scratch)? {
            size_ratio = Some(ratio);
            if mult == 0.0 {
                continue; // payload size too divergent — drop
            }
            score *= mult;
        }

        if score < cfg.merged_floor {
            continue;
        }
        out.push(FuzzyCandidate {
            redump_id: scratch.row.redump_id,
            system: scratch.row.system,
            title: scratch.row.title,
            redump_url: scratch.row.redump_url,
            score,
            sources: scratch.sources,
            size_ratio,
            inferred_version: scratch.inferred_version,
            pvd_creation_date: scratch.row.pvd_creation_date,
            match_reason: scratch.reason,
        });
    }

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.redump_id.cmp(&b.redump_id))
    });
    out.truncate(cfg.candidate_cap);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_basics() {
        assert_eq!(normalize("QUAKE_106"), "quake 106");
        assert_eq!(normalize("  Space   Quest  "), "space quest");
        assert_eq!(normalize("Command & Conquer"), "command conquer");
    }

    #[test]
    fn version_suffix() {
        assert_eq!(strip_version_suffix("QUAKE_106"), ("QUAKE".into(), Some("1.06".into())));
        assert_eq!(strip_version_suffix("DOOM2_19"), ("DOOM2".into(), Some("1.9".into())));
        assert_eq!(strip_version_suffix("MYST_UK"), ("MYST_UK".into(), None));
        assert_eq!(strip_version_suffix("PLAINLABEL"), ("PLAINLABEL".into(), None));
    }

    #[test]
    fn roman_numerals() {
        assert_eq!(roman_to_arabic("II"), Some(2));
        assert_eq!(roman_to_arabic("IV"), Some(4));
        assert_eq!(roman_to_arabic("VIII"), Some(8));
        assert_eq!(roman_to_arabic("turbo"), None);
    }

    #[test]
    fn acronyms() {
        assert_eq!(build_acronym("Super Street Fighter II Turbo"), "ssf2t");
        assert_eq!(build_acronym("Space Quest V"), "sq5");
        assert_eq!(build_acronym("Space Quest 6"), "sq6");
        assert_eq!(build_acronym("Doom"), "d");
    }

    #[test]
    fn acronym_uses_pre_subtitle_segment() {
        let acs = title_acronyms("Space Quest 6: Roger Wilco: The Spinal Frontier");
        assert!(acs.contains(&"sq6".to_string()));
    }

    #[test]
    fn acronym_matches_via_best_title_score() {
        let (score, matcher) = best_title_score("ssf2t", "Super Street Fighter II Turbo");
        assert_eq!(matcher, "acronym");
        assert_eq!(score, 1.0);
    }

    #[test]
    fn token_set_handles_reorder() {
        assert!(token_set_ratio("Quake Arena III", "Quake III: Arena") > 0.9);
    }

    #[test]
    fn token_set_rewards_subset_subtitle() {
        // ≥2 shared tokens subset → still rewarded.
        assert!(
            token_set_ratio(
                "Space Quest 6",
                "Space Quest 6: Roger Wilco: The Spinal Frontier"
            ) > 0.95
        );
    }

    #[test]
    fn generic_disc_words_do_not_count_as_corroborating_tokens() {
        // "Canada Disc" volume label vs an X-Plane title that also contains
        // "Canada" and "Disc". Without filtering, both "canada" and "disc"
        // would count as a 2-token subset and ride the subset reward to ~1.0.
        // With filtering, only "canada" survives → single-token subset → no
        // reward → score is dominated by length-aware full-set compare and
        // stays well below the floor.
        let s = token_set_ratio(
            "Canada Disc",
            "X-Plane 9 (Disc 5) (Canada & Arctic Regions Scenery)",
        );
        assert!(s < 0.65, "expected sub-floor, got {s}");
    }

    #[test]
    fn letter_digit_split_basic() {
        assert_eq!(crate::disc::content::letter_digit_split("QUAKE106"), "QUAKE 106");
        assert_eq!(crate::disc::content::letter_digit_split("TR1"), "TR 1");
        assert_eq!(crate::disc::content::letter_digit_split("MK3PCCDROM"), "MK 3 PCCDROM");
        // pure letters / digits unchanged
        assert_eq!(crate::disc::content::letter_digit_split("Discworld"), "Discworld");
        assert_eq!(crate::disc::content::letter_digit_split("123"), "123");
        assert_eq!(crate::disc::content::letter_digit_split("ABC"), "ABC");
    }

    #[test]
    fn camel_split_basic() {
        // CamelCase only splits at case boundaries — digit boundaries are
        // letter_digit_split's job. The label_variants pipeline returns both
        // variants so downstream FTS sees `mortal kombat 3` either way.
        assert_eq!(crate::disc::content::camel_split("MortalKombat3"), "Mortal Kombat3");
        assert_eq!(crate::disc::content::camel_split("GLQuake"), "GL Quake");
        // Tradeoff: the upper-run-to-camel rule splits at the LAST upper of a
        // run, so `AOEmac` becomes `AO Emac`. Acceptable — the alternative
        // (`AOE mac`) breaks `GLQuake`. AOE-style cases are uncommon and
        // typically use a separator anyway (`AOE-mac.cue`).
        assert_eq!(crate::disc::content::camel_split("AOEmac"), "AO Emac");
        assert_eq!(crate::disc::content::camel_split("Discworld"), "Discworld");
        assert_eq!(crate::disc::content::camel_split("ABC"), "ABC");
    }

    #[test]
    fn label_variants_mortal_kombat_3() {
        // Combined camel + digit splits via label_variants. Any of the
        // returned variants should yield the right tokens.
        let v = label_variants("MortalKombat3");
        // camel_split → "Mortal Kombat3", letter_digit_split → "MortalKombat 3"
        // Their token sets, unioned, contain {mortal, kombat, 3}.
        // Chained variant fully separates: "Mortal Kombat 3".
        assert!(v.iter().any(|s| s == "Mortal Kombat 3"), "got {:?}", v);
    }

    #[test]
    fn label_variants_quake_106() {
        let v = label_variants("QUAKE_106");
        // Should include version-stripped stem and letter/digit variant.
        assert!(v.iter().any(|s| s == "QUAKE"));
    }

    #[test]
    fn label_variants_keeps_discworld_intact() {
        let v = label_variants("Discworld");
        // The only variant is the original (no boundaries to split).
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], "Discworld");
    }

    #[test]
    fn discworld_is_not_treated_as_generic() {
        // Make sure we strip "disc" the word, not the prefix. "Discworld" is
        // a single alphanumeric token so it survives untouched.
        let s = token_set_ratio("Discworld", "Discworld");
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cdrom_dvdrom_filtered() {
        // "CD_ROM" volume label tokenizes to "cd" + "rom"; both are generic.
        // Without filtering, this would 100%-subset-match every redump title
        // containing "CD-ROM". With filtering, it has zero distinctive tokens
        // and matches nothing.
        let s = token_set_ratio("CD_ROM", "Trivial Pursuit: CD-ROM Edition");
        assert!(s < 0.5, "expected very low, got {s}");
    }

    #[test]
    fn token_set_demotes_single_token_subset() {
        // One shared common word must NOT score ~1.0 against a long title.
        assert!(token_set_ratio("Rescue", "Barbie: Pet Rescue") < 0.7);
        assert!(token_set_ratio("test", "Test Drive 5") < 0.7);
        assert!(token_set_ratio("MISSION", "Mission to McDonaldland") < 0.7);
    }

    #[test]
    fn token_set_exact_single_token_still_full() {
        // A genuine exact single-token match stays 1.0 (via full-set compare).
        assert!((token_set_ratio("Quake", "Quake") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn substring_partial() {
        let s = substring_containment("quake", "quake ii");
        assert!(s > 0.0 && s < 1.0);
        assert_eq!(substring_containment("ab", "abcdef"), 0.0); // too short
    }

    #[test]
    fn align_tracks_bonus_edition() {
        // 12 identical tracks + 1 extra on the candidate side → 12/13.
        let disc: Vec<u32> = (0..12).map(|i| 1000 + i * 100).collect();
        let mut redump = disc.clone();
        redump.push(150); // bonus track
        let score = align_tracks(&disc, &redump, 150);
        assert!((score - 12.0 / 13.0).abs() < 1e-9);
    }

    #[test]
    fn align_tracks_exact() {
        let disc = vec![1000u32, 2000, 3000];
        assert_eq!(align_tracks(&disc, &disc.clone(), 150), 1.0);
    }
}
