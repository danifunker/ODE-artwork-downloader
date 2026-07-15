//! End-to-end check against the real `latest` release of ODE-lookup-db.
//!
//! This test hits the network and writes to a temp directory; it's `#[ignore]`
//! so it doesn't run by default. Run with:
//!   `cargo test --test db_update_e2e -- --ignored --nocapture`

use std::env;

use ode_artwork_downloader::db::{lookup, DatabaseManager, UpdateOutcome};

#[test]
#[ignore]
fn updates_and_opens_real_release() {
    // Redirect ProjectDirs::data_dir() to a temp location for the duration of
    // the test by overriding XDG/AppData env vars. directories::ProjectDirs
    // respects these on Linux/Windows; on macOS it ignores them, so we just
    // accept that on macOS this test touches the real cache dir.
    let temp = tempfile::tempdir().unwrap();
    if cfg!(target_os = "linux") {
        env::set_var("XDG_DATA_HOME", temp.path());
    } else if cfg!(target_os = "windows") {
        env::set_var("APPDATA", temp.path());
    }

    let mgr = DatabaseManager::new().expect("manager init");

    let outcome = mgr.update_if_needed().expect("update");
    match &outcome {
        UpdateOutcome::Updated { local_path, .. } => {
            assert!(local_path.exists(), "DB file should exist after update");
        }
        UpdateOutcome::UpToDate { local_path } => {
            assert!(local_path.exists());
        }
        UpdateOutcome::OfflineUsingCached { .. } | UpdateOutcome::OfflineNoCache { .. } => {
            panic!("network must be available for this test; got {outcome:?}");
        }
    }

    let conn = mgr.open().expect("open");

    let (schema_version, row_count): (i64, i64) = conn
        .query_row(
            "SELECT schema_version, row_count FROM meta WHERE source = 'redump'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("meta query");
    assert_eq!(schema_version, 3);
    assert!(row_count > 0);

    let discs: i64 = conn
        .query_row("SELECT COUNT(*) FROM redump_disc", [], |row| row.get(0))
        .unwrap();
    assert_eq!(discs, row_count);

    let fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM redump_disc_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts, row_count);

    // Any file row at all should be reachable by its sha1. Go through the real
    // lookup rather than a parallel query, so that a hash moving tables again
    // fails here instead of passing against a table nothing reads.
    let (some_sha1,): (String,) = conn
        .query_row(
            "SELECT sha1 FROM redump_file WHERE sha1 IS NOT NULL LIMIT 1",
            [],
            |row| Ok((row.get(0)?,)),
        )
        .unwrap();
    let hits = lookup::by_track_sha1(&conn, &some_sha1).expect("sha1 lookup");
    assert!(!hits.is_empty(), "round-trip sha1 lookup should hit");

    // A disc lists the same hash on more than one file — every CD's whole-disc
    // `.img` mirror repeats the crc32 of its `.bin`. The lookup must still yield
    // each disc once. (CRC32 genuinely collides across unrelated discs, so this
    // checks for repeats rather than a total count.)
    let (dupe_crc32,): (String,) = conn
        .query_row(
            "SELECT crc32 FROM redump_file WHERE crc32 IS NOT NULL \
             GROUP BY redump_id, crc32 HAVING COUNT(*) > 1 LIMIT 1",
            [],
            |row| Ok((row.get(0)?,)),
        )
        .expect("v3 has discs whose files repeat a crc32");
    let hits = lookup::by_track_crc32(&conn, &dupe_crc32).expect("crc32 lookup");
    let mut ids: Vec<i64> = hits.iter().map(|h| h.redump_id).collect();
    let found = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(found, ids.len(), "each disc must be returned only once");

    // Geometry-only track table: DVDs contribute no rows at all.
    let dvds_with_tracks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM redump_disc d WHERE d.media LIKE 'DVD%' \
             AND EXISTS (SELECT 1 FROM redump_track t WHERE t.redump_id = d.redump_id)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dvds_with_tracks, 0, "v3 DVDs have no track rows");
}
