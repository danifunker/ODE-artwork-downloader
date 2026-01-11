//! Disc image handling module
//!
//! Provides functionality for reading various disc image formats and extracting
//! volume labels and other identifying information.

mod bincue;
mod chd;
mod formats;
mod identifier;
mod iso9660;
mod reader;

// Public API re-exports (some may be unused until later phases)
#[allow(unused_imports)]
pub use formats::{DiscFormat, FilesystemType, supported_extensions};
#[allow(unused_imports)]
pub use identifier::{parse_filename, normalize_volume_label, ConfidenceLevel, ParsedFilename};
#[allow(unused_imports)]
pub use iso9660::PrimaryVolumeDescriptor;
#[allow(unused_imports)]
pub use reader::{DiscInfo, DiscReader, DiscError};
