//! End-to-end check against the real `latest` release of ODE-lookup-db.
//!
//! This test hits the network and writes to a temp directory; it's `#[ignore]`
//! so it doesn't run by default. Run with:
//!   `cargo test --test db_update_e2e -- --ignored --nocapture`

use std::env;

use ode_artwork_downloader::db::{DatabaseManager, UpdateOutcome};

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
    assert_eq!(schema_version, 2);
    assert!(row_count > 0);

    let discs: i64 = conn
        .query_row("SELECT COUNT(*) FROM redump_disc", [], |row| row.get(0))
        .unwrap();
    assert_eq!(discs, row_count);

    let fts: i64 = conn
        .query_row("SELECT COUNT(*) FROM redump_disc_fts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts, row_count);

    // Any track row at all should be reachable by its sha1.
    let (some_sha1,): (String,) = conn
        .query_row(
            "SELECT sha1 FROM redump_track WHERE sha1 IS NOT NULL LIMIT 1",
            [],
            |row| Ok((row.get(0)?,)),
        )
        .unwrap();
    let hit: Option<i64> = conn
        .query_row(
            "SELECT redump_id FROM redump_track WHERE sha1 = lower(?1) LIMIT 1",
            [&some_sha1],
            |row| row.get(0),
        )
        .ok();
    assert!(hit.is_some(), "round-trip sha1 lookup should hit");
}
