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
    version, category, media, barcode, pvd_volume_id, pvd_creation_date, redump_url";

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
        pvd_volume_id: row.get(9)?,
        pvd_creation_date: row.get(10)?,
        redump_url: row.get(11)?,
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

pub fn by_track_sha1(conn: &Connection, sha1: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} FROM discs d \
         JOIN tracks t USING (redump_id) WHERE t.sha1 = ?1"
    );
    query_via_join(conn, &sql, sha1, MatchSource::TrackSha1)
}

pub fn by_track_md5(conn: &Connection, md5: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} FROM discs d \
         JOIN tracks t USING (redump_id) WHERE t.md5 = ?1"
    );
    query_via_join(conn, &sql, md5, MatchSource::TrackMd5)
}

pub fn by_track_crc32(conn: &Connection, crc32: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} FROM discs d \
         JOIN tracks t USING (redump_id) WHERE t.crc32 = ?1"
    );
    query_via_join(conn, &sql, crc32, MatchSource::TrackCrc32)
}

pub fn by_serial(conn: &Connection, serial: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!(
        "SELECT {SELECT_DISCS_COLS} FROM discs d \
         JOIN serials s USING (redump_id) WHERE s.serial = ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows =
        stmt.query_map([serial], |row| row_to_match(row, MatchSource::Serial))?;
    rows.collect()
}

pub fn by_barcode(conn: &Connection, barcode: &str) -> rusqlite::Result<Vec<RedumpMatch>> {
    let sql = format!("SELECT {SELECT_DISCS_COLS} FROM discs WHERE barcode = ?1");
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
        "SELECT {SELECT_DISCS_COLS} FROM discs \
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
         FROM discs_fts \
         JOIN discs d ON d.redump_id = discs_fts.rowid \
         WHERE discs_fts MATCH ?1 \
         ORDER BY bm25(discs_fts) \
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
    if let Some(crc32) = inputs.track_crc32 {
        let hits = by_track_crc32(conn, crc32)?;
        if !hits.is_empty() {
            return Ok(Some(hits));
        }
    }
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

/// Lookup a single disc by its `redump_id`. Returns `None` if not present.
#[allow(dead_code)]
pub fn by_redump_id(conn: &Connection, redump_id: i64) -> rusqlite::Result<Option<RedumpMatch>> {
    let sql = format!("SELECT {SELECT_DISCS_COLS} FROM discs WHERE redump_id = ?1");
    conn.query_row(&sql, [redump_id], |row| {
        row_to_match(row, MatchSource::PvdVolumeId)
    })
    .optional()
}
