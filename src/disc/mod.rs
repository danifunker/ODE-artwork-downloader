//! Disc image handling module
//!
//! Provides functionality for reading various disc image formats and extracting
//! volume labels and other identifying information.

mod apm;
mod bincue;
pub mod browse;
mod chd;
mod formats;
mod hfs;
mod hfsplus;
mod identifier;
mod iso9660;
mod reader;
mod toc;

// Public API re-exports (some may be unused until later phases)
#[allow(unused_imports)]
pub use formats::{DiscFormat, FilesystemType, supported_extensions};
#[allow(unused_imports)]
pub use identifier::{parse_filename, normalize_volume_label, ConfidenceLevel, ParsedFilename};
#[allow(unused_imports)]
pub use iso9660::PrimaryVolumeDescriptor;
#[allow(unused_imports)]
pub use reader::{DiscInfo, DiscReader, DiscError, set_log_callback, clear_log_callback};
#[allow(unused_imports)]
pub use toc::{DiscTOC, TrackInfo};
#[allow(unused_imports)]
pub use hfs::MasterDirectoryBlock;
#[allow(unused_imports)]
pub use hfsplus::HfsPlusVolumeHeader;
#[allow(unused_imports)]
pub use apm::PartitionEntry;
