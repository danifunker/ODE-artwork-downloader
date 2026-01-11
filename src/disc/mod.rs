//! Disc image handling module
//!
//! Provides functionality for reading various disc image formats and extracting
//! volume labels and other identifying information.

mod formats;
mod identifier;
mod iso9660;
mod reader;

pub use formats::{DiscFormat, FilesystemType};
pub use identifier::{parse_filename, ConfidenceLevel};
pub use iso9660::PrimaryVolumeDescriptor;
pub use reader::{DiscInfo, DiscReader, DiscError};
