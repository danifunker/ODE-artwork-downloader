//! Disc image handling module
//!
//! Provides functionality for reading various disc image formats and extracting
//! volume labels and other identifying information.
//!
//! Format/filesystem parsing is delegated to the `opticaldiscs` library.
//! ODE-specific logic (game title parsing, confidence scoring) lives here.

pub mod browse;
pub mod hasher;
mod identifier;
mod reader;

// Re-exports from opticaldiscs
pub use opticaldiscs::formats::{supported_extensions, DiscFormat, FilesystemType};
pub use opticaldiscs::hfs::MasterDirectoryBlock;
pub use opticaldiscs::hfsplus::HfsPlusVolumeHeader;
pub use opticaldiscs::iso9660::PrimaryVolumeDescriptor;
pub use opticaldiscs::toc::{DiscTOC, TrackInfo};

// ODE-specific re-exports
pub use identifier::{normalize_volume_label, parse_filename, ConfidenceLevel, ParsedFilename};
pub use reader::{clear_log_callback, set_log_callback, DiscError, DiscInfo, DiscReader};
