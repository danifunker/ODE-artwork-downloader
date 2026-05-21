//! Redump lookup database (downloaded SQLite artifact).
//!
//! See `docs/dbintegration.md` for the overall plan. This module handles the
//! download/verify/swap flow and read-only opening; query helpers live in
//! `lookup.rs` (added when the lookup cascade is wired up).

mod fetch;
pub mod fuzzy;
pub mod lookup;
pub mod manager;
mod paths;
mod seed;
pub mod verify;

pub use fuzzy::{fuzzy_search, FuzzyCandidate, FuzzyInputs, ScoreSource};
pub use verify::{classify as classify_one, gather_evidence, verify as verify_candidates, DiscEvidence, Verdict};
pub use lookup::{
    cascade, cascade_from_disc, fuzzy_from_disc, CascadeInputs, MatchSource, RedumpMatch,
};
pub use manager::{DatabaseManager, UpdateOutcome};

/// Schema version this build of the app understands.
pub const SUPPORTED_SCHEMA_VERSION: i64 = 1;

/// Base URL for the `latest` release tag on the DB repo.
pub const LATEST_RELEASE_BASE: &str =
    "https://github.com/danifunker/ODE-lookup-db/releases/download/latest";

/// HTTP User-Agent sent with all DB-related requests.
pub const USER_AGENT: &str = concat!(
    "ODE-Artwork-Downloader/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/danifunker/ODE-artwork-downloader)"
);
