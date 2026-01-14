//! Disc image handling module
//!
//! Provides functionality for reading various disc image formats and extracting
//! volume labels and other identifying information.

mod apm;
mod bincue;
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

use std::io::{Read, Seek, SeekFrom};

/// A reader that wraps another reader and applies a start offset.
/// All seek and read operations are relative to this start offset.
struct OffsetReader<R: Read + Seek> {
    inner: R,
    start_offset: u64,
}

impl<R: Read + Seek> OffsetReader<R> {
    fn new(mut inner: R, start_offset: u64) -> std::io::Result<Self> {
        inner.seek(SeekFrom::Start(start_offset))?;
        Ok(Self { inner, start_offset })
    }
}

impl<R: Read + Seek> Read for OffsetReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Read + Seek> Seek for OffsetReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => self.start_offset + p,
            _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Unsupported seek")),
        };
        self.inner.seek(SeekFrom::Start(new_pos)).map(|p| p - self.start_offset)
    }
}
