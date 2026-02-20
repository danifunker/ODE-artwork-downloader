//! Disc filesystem browsing module
//!
//! Delegates all format/filesystem browsing to the `opticaldiscs` library.

// Re-export everything the rest of ODE needs from opticaldiscs
pub use opticaldiscs::browse::entry::{EntryType, FileEntry};
pub use opticaldiscs::browse::filesystem::{Filesystem, FilesystemError};
pub use opticaldiscs::browse::open_disc_filesystem;

use crate::disc::DiscInfo;

/// Open a filesystem from disc info.
///
/// Wraps `opticaldiscs::browse::open_disc_filesystem`, converting the ODE
/// `DiscInfo` to the opticaldiscs `DiscImageInfo` via a temporary probe.
pub fn open_filesystem(disc_info: &DiscInfo) -> Result<Box<dyn Filesystem>, FilesystemError> {
    // Re-open via opticaldiscs to get the DiscImageInfo the browse layer needs.
    // This is a lightweight probe (sector reads only, no full parse).
    let odi = opticaldiscs::detect::DiscImageInfo::open(&disc_info.path)
        .map_err(|e| FilesystemError::Parse(e.to_string()))?;
    open_disc_filesystem(&odi)
}
