//! Update / open lifecycle for the redump SQLite cache.

use std::fs;
use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

use super::fetch::{
    build_client, check_compressed, check_decompressed, decompress_with_hash, download_with_hash,
    fetch_sha256, FetchError, Urls,
};
use super::paths::DbPaths;
use super::seed::{self, SeedOutcome};
use super::SUPPORTED_SCHEMA_VERSION;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("paths: {0}")]
    Paths(String),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("schema_version {found} is newer than supported {supported}; please update the app")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("smoke test failed: {0}")]
    SmokeFailed(String),
    #[error("no database installed yet")]
    NotInstalled,
}

#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// Remote hash matches the cached one; nothing was downloaded.
    UpToDate { local_path: PathBuf },
    /// Downloaded, verified, and swapped in.
    Updated {
        local_path: PathBuf,
        compressed_sha256: String,
    },
    /// Couldn't reach the upstream, but a usable local copy exists.
    OfflineUsingCached {
        local_path: PathBuf,
        error: String,
    },
    /// Couldn't reach the upstream and no local copy exists.
    OfflineNoCache { error: String },
}

pub struct DatabaseManager {
    paths: DbPaths,
}

impl DatabaseManager {
    pub fn new() -> Result<Self, DbError> {
        let paths = DbPaths::discover().map_err(DbError::Paths)?;
        Ok(Self { paths })
    }

    pub fn sqlite_path(&self) -> PathBuf {
        self.paths.sqlite()
    }

    /// Check upstream, download + verify + swap if changed. Safe to call on
    /// startup; falls back to the cached file if the network is unavailable.
    pub fn update_if_needed(&self) -> Result<UpdateOutcome, DbError> {
        // One-shot upgrade cleanup: discard the pre-unified `redump.sqlite`
        // cache so a stale schema/file doesn't sit next to the new one.
        for legacy in self.paths.legacy_artifacts() {
            if legacy.exists() {
                if let Err(e) = fs::remove_file(&legacy) {
                    log::warn!("Could not remove legacy {}: {e}", legacy.display());
                } else {
                    log::info!("Removed legacy DB artifact {}", legacy.display());
                }
            }
        }

        // Unpack the embedded seed (if any) before touching the network. This
        // gives offline first-run users a working DB.
        match seed::try_install_if_missing(&self.paths) {
            Ok(SeedOutcome::Installed { bytes }) => {
                log::info!("Installed embedded ODE-lookup DB seed ({bytes} bytes)");
            }
            Ok(SeedOutcome::AlreadyInstalled) | Ok(SeedOutcome::NotEmbedded) => {}
            Err(e) => log::warn!("Embedded seed install failed: {e}"),
        }

        let urls = Urls::latest();
        let client = match build_client() {
            Ok(c) => c,
            Err(e) => return Ok(self.offline_outcome(e.to_string())),
        };

        let remote_zst_hash = match fetch_sha256(&client, &urls.zst_sha256) {
            Ok(h) => h,
            Err(e) => return Ok(self.offline_outcome(e.to_string())),
        };

        if self.is_up_to_date(&remote_zst_hash) {
            log::info!("Lookup DB cache is up to date ({})", &remote_zst_hash[..12]);
            return Ok(UpdateOutcome::UpToDate {
                local_path: self.paths.sqlite(),
            });
        }

        let remote_plain_hash = fetch_sha256(&client, &urls.plain_sha256)?;

        let tmp_zst = self.paths.download_tmp();
        let tmp_plain = self.paths.decompress_tmp();

        // Always start clean — a prior partial run may have left stragglers.
        let _ = fs::remove_file(&tmp_zst);
        let _ = fs::remove_file(&tmp_plain);

        log::info!("Downloading lookup DB: {}", urls.zst);
        let got_zst_hash = download_with_hash(&client, &urls.zst, &tmp_zst)?;
        check_compressed(&remote_zst_hash, &got_zst_hash)?;

        log::info!("Decompressing lookup DB");
        let got_plain_hash = decompress_with_hash(&tmp_zst, &tmp_plain)?;
        check_decompressed(&remote_plain_hash, &got_plain_hash)?;

        // Open and smoke-test before swapping in. If anything fails here we
        // leave the live DB untouched and the tmp files in place for debugging
        // (next run will clobber them anyway).
        smoke_test(&tmp_plain)?;

        // Atomic swap. On Windows std::fs::rename refuses to overwrite, so we
        // remove the existing target first; the small race window is fine
        // because nothing else writes here.
        let final_path = self.paths.sqlite();
        if final_path.exists() {
            let _ = fs::remove_file(&final_path);
        }
        fs::rename(&tmp_plain, &final_path)?;
        let _ = fs::remove_file(&tmp_zst);
        fs::write(self.paths.last_zst_sha256(), &remote_zst_hash)?;

        Ok(UpdateOutcome::Updated {
            local_path: final_path,
            compressed_sha256: remote_zst_hash,
        })
    }

    /// Open the cached DB read-only. Errors if no DB has been installed yet.
    pub fn open(&self) -> Result<Connection, DbError> {
        let path = self.paths.sqlite();
        if !path.exists() {
            return Err(DbError::NotInstalled);
        }
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_schema_supported(&conn)?;
        Ok(conn)
    }

    fn is_up_to_date(&self, remote_zst_hash: &str) -> bool {
        if !self.paths.sqlite().exists() {
            return false;
        }
        match fs::read_to_string(self.paths.last_zst_sha256()) {
            Ok(local) => local.trim().eq_ignore_ascii_case(remote_zst_hash),
            Err(_) => false,
        }
    }

    fn offline_outcome(&self, error: String) -> UpdateOutcome {
        let path = self.paths.sqlite();
        if path.exists() {
            UpdateOutcome::OfflineUsingCached {
                local_path: path,
                error,
            }
        } else {
            UpdateOutcome::OfflineNoCache { error }
        }
    }
}

fn assert_schema_supported(conn: &Connection) -> Result<(), DbError> {
    // Unified DB has one meta row per source; we only require the redump
    // schema to be readable. winworld schema is checked separately if/when
    // the winworld tables are queried.
    let found: i64 = conn.query_row(
        "SELECT schema_version FROM meta WHERE source = 'redump'",
        [],
        |row| row.get(0),
    )?;
    if found > SUPPORTED_SCHEMA_VERSION {
        return Err(DbError::SchemaTooNew {
            found,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Lightweight assertions that the artifact is the shape we expect. Runs on
/// every successful download (not on plain `open`) so we never swap in a broken
/// DB.
fn smoke_test(path: &PathBuf) -> Result<(), DbError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    assert_schema_supported(&conn)?;

    let (declared_row_count, _built_at): (i64, String) = conn
        .query_row(
            "SELECT row_count, built_at FROM meta WHERE source = 'redump'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

    let discs_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM redump_disc", [], |row| row.get(0))?;
    if discs_count != declared_row_count {
        return Err(DbError::SmokeFailed(format!(
            "redump_disc count {discs_count} != meta.row_count {declared_row_count}"
        )));
    }

    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM redump_disc_fts", [], |row| row.get(0))?;
    if fts_count != declared_row_count {
        return Err(DbError::SmokeFailed(format!(
            "redump_disc_fts count {fts_count} != meta.row_count {declared_row_count}"
        )));
    }

    // The release ships with ≥1 disc; if the count is zero, something is wrong.
    if declared_row_count == 0 {
        return Err(DbError::SmokeFailed("meta.row_count is zero".to_string()));
    }

    Ok(())
}
