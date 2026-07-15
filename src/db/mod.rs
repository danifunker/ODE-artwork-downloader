//! ODE-lookup database (downloaded SQLite artifact).
//!
//! Bundles `redump` and `winworld` source tables in one SQLite file
//! (`ode-lookup.sqlite`). The redump side carries the exact-match cascade
//! (track hashes, serials, PVD) and the redump-side fuzzy sources. The
//! winworld side feeds a complementary fuzzy source for applications /
//! operating systems that redump doesn't cover.
//!
//! See `MIGRATION-unified-db.md` in ODE-lookup-db for the breaking-change
//! details and `docs/dbintegration.md` for the overall plan.

mod fetch;
pub mod fuzzy;
pub mod lookup;
pub mod manager;
mod paths;
mod seed;
pub mod verify;

pub use fuzzy::{fuzzy_search, FuzzyCandidate, FuzzyInputs, ScoreSource, WinworldRef};
pub use verify::{classify as classify_one, gather_evidence, verify as verify_candidates, DiscEvidence, Verdict};
pub use lookup::{
    by_redump_id, cascade, cascade_from_disc, fuzzy_from_disc, CascadeInputs, MatchSource,
    RedumpMatch,
};
pub use manager::{DatabaseManager, UpdateOutcome};

/// Schema version this build of the app understands.
pub const SUPPORTED_SCHEMA_VERSION: i64 = 3;

/// Oldest schema version this build can read. v3 moved the per-file hashes we
/// match on off `redump_track` and onto `redump_file`, a table that simply does
/// not exist in v2 — so an older database has to be re-downloaded rather than
/// queried and tolerated.
pub const MINIMUM_SCHEMA_VERSION: i64 = 3;

/// Base URL for the `latest` release tag on the DB repo.
pub const LATEST_RELEASE_BASE: &str =
    "https://github.com/danifunker/ODE-lookup-db/releases/download/latest";

/// HTTP User-Agent sent with all DB-related requests.
pub const USER_AGENT: &str = concat!(
    "ODE-Artwork-Downloader/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/danifunker/ODE-artwork-downloader)"
);
