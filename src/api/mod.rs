//! API and database integration module
//!
//! Provides functionality for matching disc images against game databases
//! and searching for cover artwork.

pub mod artwork;
pub mod redump;

pub use artwork::{open_in_browser, ArtworkSearchQuery};
pub use redump::{RedumpDatabase, RedumpGame};
