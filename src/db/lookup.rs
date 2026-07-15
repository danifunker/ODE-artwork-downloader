//! Lookup queries against the redump SQLite cache.
//!
//! Each function returns a `Vec<RedumpMatch>` — multiple hits are possible
//! (e.g. two regional pressings sharing a PVD volume label). Callers decide
//! how to disambiguate.

use rusqlite::{Connection, OptionalExtension, Row};

/// A single matched row from the `discs` table.
#[derive(Debug, Clone)]
pub struct RedumpMatch {
    pub redump_id: i64,
    pub system: String,
    pub title: String,
    pub foreign_title: Option<String>,
    pub edition: Option<String>,
    pub version: Option<String>,
    pub category: Option<String>,
    pub media: Option<String>,
    pub barcode: Option<String>,
    pub catalog: Option<String>,
    pub pvd_volume_id: Option<String>,
    pub pvd_creation_date: Option<String>,
    pub redump_url: String,
    /// How the row was found. Useful for UI ("matched on SHA1") and logging.
    pub matched_via: MatchSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchSource {
    TrackSha1,
    TrackMd5,
    TrackCrc32,
    Serial,
    Barcode,
    PvdVolumeId,
    FuzzyTitle,
}

const SELECT_DISCS_COLS: &str = "redump_id, system, title, foreign_title, edition, \
    version, category, media, barcode, catalog, pvd_volume_id, pvd_creation_date, redump_url";

fn row_to_match(row: &Row<'_>, matched_via: MatchSource) -> rusqlite::Result<RedumpMatch> {
    Ok(RedumpMatch {
        redump_id: row.get(0)?,
        system: row.get(1)?,
        title: row.get(2)?,
        foreign_title: row.get(3)?,
        edition: row.get(4)?,
        version: row.get(5)?,
        category: row.get(6)?,
        media: row.get(7)?,
        barcode: row.get(8)?,
        catalog: row.get(9)?,
        pvd_volume_id: row.get(10)?,
        pvd_creation_date: row.get(11)?,
        redump_url: row.get(12)?,
        matched_via,
    })
}

fn query_via_join(
    conn: &Connection,
    sql: &str,
    param: &str,
    source: MatchSource,
) -> rusqlite::Result<Vec<RedumpMatch>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([param.to_ascii_lowercase()], |row| row_to_match(row, source))?;
    rows.collect()
}

// Schema v3 moved per-file hashes off `redump_track` (now geometry-only) onto
// `redump_file` — one row per dumped file: the `.cue`, one `.bin` per track, and
// a whole-disc `.img` mirror. We still match a *track* hash the app computed
// itself; it just lands on the track's `.bin` row.
//
// DISTINCT because one disc can own several rows with the same hash: on a
// single-track disc the `.img` mirror repeats its `.bin`'s crc32, and identical
// (e.g. silent) audio tracks share a hash. We select only disc columns, so
// DISTINCT collapses those back to one row per disc.
//
// Any hash column may be NULL (redump.info omits md5/sha1 on the `.img` mirror).
// A NULL never equals the probe, so those rows simply don't match — the caller
// falls through to the next hash tier.
pub fn by_track_sha1(conn: &Connection, sha1: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT DISTINCT {SELECT_DISCS_COLS} FROM redump_disc d \
         JOIN redump_file f USING (redump_id) WHERE f.sha1 = ?1"
    );
    query_via_join(conn, &sql, sha1, MatchSource::TrackSha1)
}

pub fn by_track_md5(conn: &Connection, md5: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT DISTINCT {SELECT_DISCS_COLS} FROM redump_disc d \
         JOIN redump_file f USING (redump_id) WHERE f.md5 = ?1"
    );
    query_via_join(conn, &sql, md5, MatchSource::TrackMd5)
}

pub fn by_track_crc32(conn: &Connection, crc32: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT DISTINCT {SELECT_DISCS_COLS} FROM redump_disc d \
         JOIN redump_file f USING (redump_id) WHERE f.crc32 = ?1"
    );
    query_via_join(conn, &sql, crc32, MatchSource::TrackCrc32)
}

pub fn by_serial(conn: &Connection, serial: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} FROM redump_disc d \
         JOIN redump_serial s USING (redump_id) WHERE s.serial = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows =
        stmt.query_map([serial], |row| row_to_match(row, MatchSource::Serial))?;
    rows.collect()
}

pub fn by_barcode(conn: &Connection, barcode: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!("SELECT {SELECT_DISCS_COLS} FROM redump_disc WHERE barcode = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows =
        stmt.query_map([barcode], |row| row_to_match(row, MatchSource::Barcode))?;
    rows.collect()
}

/// PVD lookup with optional `creation_date` tiebreaker. When `creation_date`
/// is provided and matches, it disambiguates between regional pressings that
/// share a volume label.
pub fn by_pvd(
    conn: &Connection,
    volume_id: &str,
    creation_date: Option<&str>,
) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} FROM redump_disc \
         WHERE pvd_volume_id = ?1 AND (?2 IS NULL OR pvd_creation_date = ?2)"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![volume_id, creation_date],
        |row| row_to_match(row, MatchSource::PvdVolumeId),
    )?;
    rows.collect()
}

/// FTS5 candidate search. Returns up to `limit` rows ordered by BM25 score
/// (smaller is better). Caller can re-rank with edit-distance if needed.
///
/// `query` is passed through to FTS5 as-is, so callers should escape user
/// input or wrap it with `*` for prefix matching.
pub fn fuzzy_title(
    conn: &Connection,
    query: &str,
    limit: i64,
) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} \
         FROM redump_disc_fts \
         JOIN redump_disc d ON d.redump_id = redump_disc_fts.rowid \
         WHERE redump_disc_fts MATCH ?1 \
         ORDER BY bm25(redump_disc_fts) \
         LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![query, limit],
        |row| row_to_match(row, MatchSource::FuzzyTitle),
    )?;
    rows.collect()
}

/// Run the standard identification cascade. Stops at the first tier that
/// returns any rows. Returns `Ok(None)` if every tier missed.
///
/// Hash tiers are skipped silently when no hash is provided — they become
/// active once `docs/PLAN-disc-hashing.md` lands.
pub fn cascade(
    conn: &Connection,
    inputs: &CascadeInputs<'_>,
) -> rusqlite::Result<Option<Vec<RedumpMatch>>> {
    if let Some(sha1) = inputs.track_sha1 {
        let hits = by_track_sha1(conn, sha1)?;
        if !hits.is_empty() {
            return Ok(Some(hits));
        }
    }
    if let Some(md5) = inputs.track_md5 {
        let hits = by_track_md5(conn, md5)?;
        if !hits.is_empty() {
            return Ok(Some(hits));
        }
    }
    // NOTE: CRC32 is intentionally NOT an exact tier. It is only 32 bits and
    // collides — a scan found unrelated Windows/Visual Studio ISOs all matching
    // one redump entry by CRC32 alone. Any genuine match already hits via SHA1
    // or MD5 above, so CRC32 adds nothing but false positives here.
    if let Some(serial) = inputs.serial {
        let hits = by_serial(conn, serial)?;
        if !hits.is_empty() {
            return Ok(Some(hits));
        }
    }
    if let Some(barcode) = inputs.barcode {
        let hits = by_barcode(conn, barcode)?;
        if !hits.is_empty() {
            return Ok(Some(hits));
        }
    }
    if let Some(volume_id) = inputs.pvd_volume_id {
        let hits = by_pvd(conn, volume_id, inputs.pvd_creation_date)?;
        if !hits.is_empty() {
            return Ok(Some(hits));
        }
    }
    Ok(None)
}

/// Inputs to `cascade`. Borrow-friendly so callers don't have to clone strings.
#[derive(Debug, Default, Clone, Copy)]
pub struct CascadeInputs<'a> {
    pub track_sha1: Option<&'a str>,
    pub track_md5: Option<&'a str>,
    pub track_crc32: Option<&'a str>,
    pub serial: Option<&'a str>,
    pub barcode: Option<&'a str>,
    pub pvd_volume_id: Option<&'a str>,
    pub pvd_creation_date: Option<&'a str>,
}

/// Run the cascade using whatever identifiers we can pull from a `DiscInfo`.
///
/// Currently sources:
/// - serial from the parsed filename (e.g. `[SCUS-94163]`)
/// - PVD volume identifier
///
/// Track hashes are not yet computed by the app (see
/// `docs/PLAN-disc-hashing.md`); when that lands, extend `CascadeInputs` here.
pub fn cascade_from_disc(
    conn: &Connection,
    info: &crate::disc::DiscInfo,
) -> rusqlite::Result<Vec<RedumpMatch>> {
    let serial = info.parsed_filename.serial.as_deref();
    let pvd_volume_id = info
        .pvd
        .as_ref()
        .map(|p| p.volume_id.trim())
        .filter(|s| !s.is_empty());

    let inputs = CascadeInputs {
        serial,
        pvd_volume_id,
        ..Default::default()
    };

    Ok(cascade(conn, &inputs)?.unwrap_or_default())
}

/// Build fuzzy-match inputs from a disc and run the fuzzy search. Called by
/// the UI only after the exact cascade misses. Returns ranked candidates.
pub fn fuzzy_from_disc(
    conn: &Connection,
    info: &crate::disc::DiscInfo,
    cfg: &crate::config::FuzzyMatchConfig,
    deep_dig: bool,
) -> rusqlite::Result<Vec<crate::db::FuzzyCandidate>> {
    // Title candidates in priority order, de-duplicated, non-empty. For each
    // raw source string we also include label_variants (letter↔digit split,
    // CamelCase split, version-suffix strip) so labels like `QUAKE106` reach
    // `Quake` and `MortalKombat3` reaches `Mortal Kombat 3`.
    let mut titles: Vec<String> = Vec::new();
    let mut push_title = |s: &str| {
        let t = s.trim();
        if !t.is_empty() && !titles.iter().any(|e| e.eq_ignore_ascii_case(t)) {
            titles.push(t.to_string());
        }
    };
    let mut push_with_variants = |s: &str| {
        for v in crate::db::fuzzy::label_variants(s) {
            push_title(&v);
        }
    };
    if let Some(label) = info.volume_label.as_deref() {
        push_with_variants(label);
    }
    push_with_variants(&info.parsed_filename.title);
    push_with_variants(&info.parsed_filename.original);
    push_with_variants(&info.title);

    let pvd_volume_id = info
        .pvd
        .as_ref()
        .map(|p| p.volume_id.trim().to_string())
        .filter(|s| !s.is_empty());

    // Per-track durations in CD frames from the absolute TOC offsets.
    let disc_track_frames = info
        .toc
        .as_ref()
        .map(|toc| {
            let offsets = &toc.track_offsets;
            let mut frames = Vec::with_capacity(offsets.len());
            for i in 0..offsets.len() {
                let end = offsets.get(i + 1).copied().unwrap_or(toc.lead_out);
                frames.push(end.saturating_sub(offsets[i]));
            }
            frames
        })
        .unwrap_or_default();

    // Total disc payload for the size-sanity source. On a mixed-mode disc the
    // PVD `volume_space_size` counts only the ISO data track, but redump's
    // total includes the audio tracks — comparing those directly makes a real
    // game look 5–10× too small and Source D wrongly drops it. When a TOC is
    // present use the lead-out (total frames × 2352 = whole-disc bytes), which
    // is comparable to redump's per-track byte sum. Fall back to the PVD size
    // for pure-data ISOs with no TOC.
    let disc_payload_bytes = info
        .toc
        .as_ref()
        .map(|toc| toc.lead_out as u64 * 2352)
        .or_else(|| {
            info.pvd
                .as_ref()
                .map(|p| p.volume_space_size as u64 * p.logical_block_size.max(2048) as u64)
        });

    let mut inputs = crate::db::FuzzyInputs {
        title_candidates: titles,
        pvd_volume_id,
        pvd_creation_date: None,
        disc_track_frames,
        disc_payload_bytes,
    };

    // First (shallow) pass: volume label + filename derivations only — no
    // filesystem walk.
    let mut candidates = crate::db::fuzzy_search(conn, &inputs, cfg)?;

    let strong = |cands: &[crate::db::FuzzyCandidate]| {
        cands.iter().filter(|c| c.score >= cfg.strong_score).count()
    };

    // Decide whether we need to read the disc. Two triggers:
    //   - zero candidates       → dig for more (cryptic labels like TR1)
    //   - many strong candidates → dig to disambiguate / rule out
    let need_dig = candidates.is_empty() || strong(&candidates) >= cfg.min_strong_for_verify;
    if !need_dig {
        return Ok(candidates);
    }
    // Caller (e.g. CLI `--no-deep-filesystem-search`) opted out of the disc
    // walk. Return the shallow-pass candidates as-is, without enrichment or
    // content-based verification.
    if !deep_dig {
        return Ok(candidates);
    }

    // Walk the disc once; reuse the result for both candidate enrichment and
    // verification.
    let content = crate::disc::read_content(info);
    log::debug!(
        "deep-dig: files_seen={} bytes_read={} phrases={} tokens={} date={:?}",
        content.files_seen,
        content.bytes_read,
        content.phrase_candidates.len(),
        content.tokens.len(),
        content.creation_date,
    );

    // If the shallow pass found nothing, enrich candidates with on-disc phrase
    // hints (directory names, exe stems, autorun/readme labels) and re-run.
    if candidates.is_empty() && !content.phrase_candidates.is_empty() {
        let mut seen: Vec<String> = inputs.title_candidates.clone();
        for phrase in &content.phrase_candidates {
            for v in crate::db::fuzzy::label_variants(phrase) {
                if !seen.iter().any(|e| e.eq_ignore_ascii_case(&v)) {
                    seen.push(v);
                }
            }
        }
        inputs.title_candidates = seen;
        candidates = crate::db::fuzzy_search(conn, &inputs, cfg)?;
    }

    if candidates.is_empty() {
        return Ok(candidates);
    }

    // Verify against the disc evidence we already gathered.
    let evidence: crate::db::DiscEvidence = content.into();
    Ok(crate::db::verify_candidates(candidates, &evidence))
}

/// Lookup a single disc by its `redump_id`. Returns `None` if not present.
pub fn by_redump_id(conn: &Connection, redump_id: i64) -> rusqlite::Result<Option<RedumpMatch>> {
    let sql = format!("SELECT {SELECT_DISCS_COLS} FROM redump_disc WHERE redump_id = ?1");
    conn.query_row(&sql, [redump_id], |row| {
        row_to_match(row, MatchSource::PvdVolumeId)
    })
    .optional()
}
