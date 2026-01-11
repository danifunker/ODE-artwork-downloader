//! API and database integration module
//!
//! Provides functionality for matching disc images against game databases.

pub mod redump;

pub use redump::{RedumpDatabase, RedumpGame};
