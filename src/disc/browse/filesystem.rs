//! Filesystem trait for disc image browsing

use super::entry::FileEntry;
use thiserror::Error;

/// Errors that can occur during filesystem operations
#[derive(Debug, Error)]
pub enum FilesystemError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Not a directory: {0}")]
    NotADirectory(String),

    #[error("Entry not found: {0}")]
    NotFound(String),

    #[error("Filesystem error: {0}")]
    Parse(String),

    #[error("Unsupported filesystem")]
    Unsupported,

    #[error("Invalid data: {0}")]
    InvalidData(String),
}

/// Abstraction over different filesystem implementations
pub trait Filesystem: Send {
    /// Get the root directory entry
    fn root(&mut self) -> Result<FileEntry, FilesystemError>;

    /// List contents of a directory
    fn list_directory(&mut self, entry: &FileEntry) -> Result<Vec<FileEntry>, FilesystemError>;

    /// Read entire file contents
    fn read_file(&mut self, entry: &FileEntry) -> Result<Vec<u8>, FilesystemError>;

    /// Read partial file contents (for large files)
    fn read_file_range(
        &mut self,
        entry: &FileEntry,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, FilesystemError>;

    /// Get the volume label/name
    fn volume_name(&self) -> Option<&str>;
}
