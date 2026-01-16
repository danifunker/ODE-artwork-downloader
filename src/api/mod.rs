//! API and database integration module
//!
//! Provides functionality for matching disc images against game databases
//! and searching for cover artwork.

pub mod artwork;
pub mod discogs;
pub mod musicbrainz;
pub mod redump;

pub use artwork::{open_in_browser, ArtworkSearchQuery, SearchConfig, ContentType};
pub use discogs::{search_release as discogs_search, DiscogsResult};
pub use musicbrainz::{search_by_discid, MusicBrainzResult};
pub use redump::{RedumpDatabase, RedumpGame};
