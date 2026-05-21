//! Disc image reader
//!
//! Unified interface for reading disc images in various formats.
//! Delegates all format/filesystem detection to the `opticaldiscs` library.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

use opticaldiscs::detect::DiscImageInfo;
use opticaldiscs::formats::{DiscFormat, FilesystemType};
use opticaldiscs::hfs::MasterDirectoryBlock;
use opticaldiscs::hfsplus::HfsPlusVolumeHeader;
use opticaldiscs::iso9660::PrimaryVolumeDescriptor;
use opticaldiscs::toc::DiscTOC;

use super::identifier::{parse_filename, normalize_volume_label, ConfidenceLevel, ParsedFilename};

/// Callback for logging disc reading progress
pub type LogCallback = Arc<Mutex<dyn FnMut(String) + Send>>;

thread_local! {
    static LOG_CALLBACK: std::cell::RefCell<Option<LogCallback>> = std::cell::RefCell::new(None);
}

/// Set a logging callback for disc reading operations
pub fn set_log_callback(callback: LogCallback) {
    LOG_CALLBACK.with(|cb| {
        *cb.borrow_mut() = Some(callback);
    });
}

/// Clear the logging callback
pub fn clear_log_callback() {
    LOG_CALLBACK.with(|cb| {
        *cb.borrow_mut() = None;
    });
}

/// Log a message to both console and callback
macro_rules! disc_log {
    ($level:ident, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        log::$level!("{}", msg);
        LOG_CALLBACK.with(|cb| {
            if let Some(callback) = cb.borrow().as_ref() {
                if let Ok(mut cb) = callback.lock() {
                    cb(msg);
                }
            }
        });
    }};
}

/// Errors that can occur when reading disc images
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum DiscError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    ParseError(String),
}

impl From<opticaldiscs::error::OpticaldiscsError> for DiscError {
    fn from(e: opticaldiscs::error::OpticaldiscsError) -> Self {
        match e {
            opticaldiscs::error::OpticaldiscsError::Io(io) => DiscError::IoError(io),
            opticaldiscs::error::OpticaldiscsError::NotFound(msg) => {
                DiscError::FileNotFound(PathBuf::from(msg))
            }
            opticaldiscs::error::OpticaldiscsError::UnsupportedFormat(fmt) => {
                DiscError::UnsupportedFormat(fmt)
            }
            other => DiscError::ParseError(other.to_string()),
        }
    }
}

/// Information extracted from a disc image
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DiscInfo {
    /// Path to the disc image file
    pub path: PathBuf,
    /// Detected disc format
    pub format: DiscFormat,
    /// Detected filesystem type
    pub filesystem: FilesystemType,
    /// Volume label (if available)
    pub volume_label: Option<String>,
    /// Parsed filename information
    pub parsed_filename: ParsedFilename,
    /// Best guess at game title
    pub title: String,
    /// Confidence level of identification
    pub confidence: ConfidenceLevel,
    /// Primary Volume Descriptor (if available)
    pub pvd: Option<PrimaryVolumeDescriptor>,
    /// Table of Contents (for audio CDs)
    pub toc: Option<DiscTOC>,
    /// HFS Master Directory Block (if HFS filesystem detected)
    pub hfs_mdb: Option<MasterDirectoryBlock>,
    /// HFS+ Volume Header (if HFS+ filesystem detected)
    pub hfsplus_header: Option<HfsPlusVolumeHeader>,
    /// Redump database matches, populated by the caller after a successful
    /// read. `None` means no lookup was attempted; `Some(vec![])` means it was
    /// attempted and found nothing.
    pub redump_matches: Option<Vec<crate::db::RedumpMatch>>,
    /// Ranked fuzzy candidates, populated only when the exact cascade misses.
    /// `None` = not attempted; `Some(vec![])` = attempted, nothing cleared the
    /// floor.
    pub fuzzy_matches: Option<Vec<crate::db::FuzzyCandidate>>,
}

impl DiscInfo {
    /// Get the output filename for cover art (same name as disc image with .jpg extension)
    pub fn cover_art_path(&self) -> PathBuf {
        self.path.with_extension("jpg")
    }

    /// Check if cover art already exists for this disc
    pub fn has_cover_art(&self) -> bool {
        self.cover_art_path().exists()
    }
}

/// Reader for disc images
pub struct DiscReader;

impl DiscReader {
    /// Read disc information from a file path
    pub fn read(path: &Path) -> Result<DiscInfo, DiscError> {
        if !path.exists() {
            return Err(DiscError::FileNotFound(path.to_path_buf()));
        }

        let parsed_filename = parse_filename(path);

        disc_log!(info, "Opening disc image: {}", path.display());

        match DiscImageInfo::open(path) {
            Ok(info) => {
                disc_log!(
                    info,
                    "Filesystem: {:?}, Volume label: {:?}",
                    info.filesystem,
                    info.volume_label
                );

                let (title, confidence) = if let Some(ref label) = info.volume_label {
                    if label.len() > 2 && !label.chars().all(|c| c.is_ascii_digit()) {
                        (normalize_volume_label(label), ConfidenceLevel::High)
                    } else {
                        (parsed_filename.title.clone(), ConfidenceLevel::Low)
                    }
                } else {
                    (parsed_filename.title.clone(), ConfidenceLevel::Low)
                };

                Ok(DiscInfo {
                    path: path.to_path_buf(),
                    format: info.format,
                    filesystem: info.filesystem,
                    volume_label: info.volume_label,
                    parsed_filename,
                    title,
                    confidence,
                    pvd: info.pvd,
                    toc: info.toc,
                    hfs_mdb: info.hfs_mdb,
                    hfsplus_header: info.hfsplus_header,
                    redump_matches: None,
                    fuzzy_matches: None,
                })
            }
            Err(opticaldiscs::error::OpticaldiscsError::UnsupportedFormat(fmt)) => {
                Err(DiscError::UnsupportedFormat(fmt))
            }
            Err(e) => {
                // For other errors (parse errors, no data track, etc.), fall back
                // to filename-only identification so the UI still shows something.
                disc_log!(warn, "Could not fully read disc ({}), using filename only", e);

                let format = DiscFormat::from_path(path).ok_or_else(|| {
                    DiscError::UnsupportedFormat(
                        path.extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("unknown")
                            .to_string(),
                    )
                })?;

                Ok(DiscInfo {
                    path: path.to_path_buf(),
                    format,
                    filesystem: FilesystemType::Unknown,
                    volume_label: None,
                    title: parsed_filename.title.clone(),
                    parsed_filename,
                    confidence: ConfidenceLevel::Low,
                    pvd: None,
                    toc: None,
                    hfs_mdb: None,
                    hfsplus_header: None,
                    redump_matches: None,
                    fuzzy_matches: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_iso() -> NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(".iso")
            .tempfile()
            .unwrap();

        // Write empty data up to PVD location
        let padding = vec![0u8; 32768]; // 16 sectors
        file.write_all(&padding).unwrap();

        // Write PVD
        let mut pvd = vec![0u8; 2048];
        pvd[0] = 1; // Type
        pvd[1..6].copy_from_slice(b"CD001"); // Identifier
        pvd[6] = 1; // Version

        // Volume ID at offset 40
        let volume_id = b"TEST_GAME                       ";
        pvd[40..72].copy_from_slice(volume_id);

        // Root directory record at offset 156 (minimal valid record)
        pvd[156] = 34; // record length
        pvd[158..162].copy_from_slice(&18u32.to_le_bytes()); // root LBA
        pvd[162..166].copy_from_slice(&18u32.to_le_bytes()); // root LBA BE
        pvd[166..170].copy_from_slice(&2048u32.to_le_bytes()); // size
        pvd[170..174].copy_from_slice(&2048u32.to_le_bytes()); // size BE
        pvd[180] = 2; // file flags: directory
        pvd[188] = 1; // file identifier length
        pvd[189] = 0; // root dot

        // Write terminator sector at sector 17
        let mut term = vec![0u8; 2048];
        term[0] = 0xFF; // terminator type
        term[1..6].copy_from_slice(b"CD001");
        term[6] = 1;
        file.write_all(&pvd).unwrap();
        file.write_all(&term).unwrap();

        file.flush().unwrap();
        file
    }

    #[test]
    fn test_read_iso() {
        let file = create_test_iso();
        let info = DiscReader::read(file.path()).unwrap();

        assert_eq!(info.format, DiscFormat::Iso);
        assert_eq!(info.filesystem, FilesystemType::Iso9660);
        assert_eq!(info.volume_label, Some("TEST_GAME".to_string()));
        assert_eq!(info.confidence, ConfidenceLevel::High);
    }

    #[test]
    fn test_file_not_found() {
        let result = DiscReader::read(Path::new("/nonexistent/path.iso"));
        assert!(matches!(result, Err(DiscError::FileNotFound(_))));
    }

    #[test]
    fn test_unsupported_format() {
        let file = tempfile::Builder::new()
            .suffix(".xyz")
            .tempfile()
            .unwrap();

        let result = DiscReader::read(file.path());
        assert!(matches!(result, Err(DiscError::UnsupportedFormat(_))));
    }
}
