//! Disc image reader
//!
//! Unified interface for reading disc images in various formats.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

use super::formats::{DiscFormat, FilesystemType};
use super::identifier::{parse_filename, ConfidenceLevel, ParsedFilename};
use super::iso9660::PrimaryVolumeDescriptor;
use super::toc::DiscTOC;

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

/// Detected disc structure type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscStructure {
    /// ISO9660 filesystem
    Iso9660,
    /// Apple Partition Map with HFS/HFS+ partitions
    ApplePartitionMap,
    /// HFS/HFS+ directly (no partition map)
    HfsDirectly,
    /// Unknown or unrecognized structure
    Unknown,
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

    #[error("CHD error: {0}")]
    ChdError(String),
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
    pub hfs_mdb: Option<super::hfs::MasterDirectoryBlock>,
    /// HFS+ Volume Header (if HFS+ filesystem detected)
    pub hfsplus_header: Option<super::hfsplus::HfsPlusVolumeHeader>,
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
    ///
    /// # Arguments
    /// * `path` - Path to the disc image file
    ///
    /// # Returns
    /// * `Ok(DiscInfo)` - Successfully read disc information
    /// * `Err(DiscError)` - Error reading or parsing the disc image
    pub fn read(path: &Path) -> Result<DiscInfo, DiscError> {
        if !path.exists() {
            return Err(DiscError::FileNotFound(path.to_path_buf()));
        }

        let format = DiscFormat::from_path(path)
            .ok_or_else(|| DiscError::UnsupportedFormat(
                path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            ))?;

        let parsed_filename = parse_filename(path);

        match format {
            DiscFormat::Iso => Self::read_iso(path, parsed_filename),
            DiscFormat::Chd => Self::read_chd(path, parsed_filename),
            DiscFormat::BinCue => Self::read_bin_cue(path, parsed_filename),
            DiscFormat::MdsMdf => Self::read_mds_mdf(path, parsed_filename),
        }
    }

    /// Read an ISO file
    fn read_iso(path: &Path, parsed_filename: ParsedFilename) -> Result<DiscInfo, DiscError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // Pre-flight check: read first 64KB to detect disc structure
        let disc_type = Self::detect_disc_structure(&mut reader)?;
        
        log::info!("Detected disc structure: {:?}", disc_type);

        // Based on detection, parse appropriately
        let (filesystem, volume_label, pvd, hfs_mdb, hfsplus_header) = match disc_type {
            DiscStructure::Iso9660 => {
                disc_log!(info, "Parsing ISO9660 filesystem");
                // Re-open for clean ISO9660 read
                let file2 = File::open(path)?;
                let mut reader2 = BufReader::new(file2);
                let pvd = PrimaryVolumeDescriptor::read_from(&mut reader2)
                    .map_err(DiscError::ParseError)?;
                
                let label = if pvd.volume_id.is_empty() {
                    None
                } else {
                    Some(pvd.volume_id.clone())
                };
                
                (FilesystemType::Iso9660, label, Some(pvd), None, None)
            }
            DiscStructure::ApplePartitionMap => {
                disc_log!(info, "Parsing Apple Partition Map");
                // Re-open for APM + HFS detection
                let file2 = File::open(path)?;
                let mut reader2 = BufReader::new(file2);
                let result = Self::try_hfs_detection(&mut reader2).unwrap_or_else(|e| {
                    disc_log!(warn, "HFS detection failed after APM: {}", e);
                    (FilesystemType::Unknown, None, None, None, None)
                });
                disc_log!(info, "APM result: filesystem={:?}, label={:?}", result.0, result.1);
                result
            }
            DiscStructure::HfsDirectly => {
                disc_log!(info, "Parsing HFS directly (no partition map)");
                // Re-open for direct HFS detection (no partition map)
                let file2 = File::open(path)?;
                let mut reader2 = BufReader::new(file2);
                
                // Seek to HFS header location
                reader2.seek(SeekFrom::Start(0))
                    .map_err(|e| DiscError::ParseError(format!("Seek failed: {}", e)))?;
                
                // Try HFS+ first
                if let Ok((header, volume_name)) = super::hfsplus::HfsPlusVolumeHeader::parse(&mut reader2) {
                    if header.is_valid() {
                        disc_log!(info, "Result: HFS+, volume: {}", volume_name);
                        (FilesystemType::HfsPlus, Some(volume_name), None, None, Some(header))
                    } else {
                        disc_log!(warn, "HFS+ header invalid");
                        (FilesystemType::Unknown, None, None, None, None)
                    }
                } else {
                    // Try HFS classic
                    reader2.seek(SeekFrom::Start(0))
                        .map_err(|e| DiscError::ParseError(format!("Seek failed: {}", e)))?;
                    
                    if let Ok(mdb) = super::hfs::MasterDirectoryBlock::parse(&mut reader2) {
                        if mdb.is_valid() {
                            disc_log!(info, "Result: HFS, volume: {}", mdb.volume_name);
                            (FilesystemType::Hfs, Some(mdb.volume_name.clone()), None, Some(mdb), None)
                        } else {
                            disc_log!(warn, "HFS MDB invalid");
                            (FilesystemType::Unknown, None, None, None, None)
                        }
                    } else {
                        disc_log!(warn, "Failed to parse HFS headers");
                        (FilesystemType::Unknown, None, None, None, None)
                    }
                }
            }
            DiscStructure::Unknown => {
                disc_log!(warn, "Unknown disc structure, no filesystem detected");
                (FilesystemType::Unknown, None, None, None, None)
            }
        };

        disc_log!(info, "Filesystem: {:?}, Volume label: {:?}", filesystem, volume_label);

        // Determine title and confidence based on available information
        let (title, confidence) = if let Some(ref label) = volume_label {
            // Use volume label if it looks like a real title
            if label.len() > 2 && !label.chars().all(|c| c.is_ascii_digit()) {
                (super::identifier::normalize_volume_label(label), ConfidenceLevel::High)
            } else {
                (parsed_filename.title.clone(), ConfidenceLevel::Low)
            }
        } else {
            (parsed_filename.title.clone(), ConfidenceLevel::Low)
        };

        Ok(DiscInfo {
            path: path.to_path_buf(),
            format: DiscFormat::Iso,
            filesystem,
            volume_label,
            parsed_filename,
            title,
            confidence,
            pvd,
            toc: None,
            hfs_mdb,
            hfsplus_header,
        })
    }

    /// Detect disc structure by reading first sectors
    fn detect_disc_structure<R: Read + Seek>(reader: &mut R) -> Result<DiscStructure, DiscError> {
        // Read first 64KB
        let mut buffer = vec![0u8; 65536];
        reader.seek(SeekFrom::Start(0))
            .map_err(|e| DiscError::IoError(e))?;
        
        let bytes_read = reader.read(&mut buffer)
            .map_err(|e| DiscError::IoError(e))?;
        
        disc_log!(info, "Read {} bytes for disc structure detection", bytes_read);
        
        if bytes_read < 2048 {
            disc_log!(warn, "File too small for disc structure detection");
            return Ok(DiscStructure::Unknown);
        }

        // Log first few bytes for debugging
        if bytes_read >= 16 {
            disc_log!(info, "First 16 bytes: {:02X?}", &buffer[0..16]);
        }
        if bytes_read >= 1040 {
            disc_log!(info, "Bytes 1024-1040: {:02X?}", &buffer[1024..1040]);
        }
        if bytes_read >= 32784 {
            disc_log!(info, "Bytes 32768-32784 (ISO PVD): {:02X?}", &buffer[32768..32784]);
        }

        // Check for Apple Partition Map at byte 0 ("ER" = 0x4552)
        if bytes_read >= 512 && buffer[0] == 0x45 && buffer[1] == 0x52 {
            disc_log!(info, "Found Apple Partition Map signature (ER) at byte 0");
            return Ok(DiscStructure::ApplePartitionMap);
        } else if bytes_read >= 2 {
            disc_log!(info, "Byte 0-1: 0x{:02X}{:02X} (not APM signature 0x4552)", buffer[0], buffer[1]);
        }

        // Check for HFS at byte 1024 ("BD" = 0x4244)
        if bytes_read >= 1026 && buffer[1024] == 0x42 && buffer[1025] == 0x44 {
            disc_log!(info, "Found HFS signature (BD) at byte 1024");
            return Ok(DiscStructure::HfsDirectly);
        } else if bytes_read >= 1026 {
            disc_log!(info, "Byte 1024-1025: 0x{:02X}{:02X} (not HFS signature 0x4244)", buffer[1024], buffer[1025]);
        }

        // Check for HFS+ at byte 1024 ("H+" = 0x482B or "HX" = 0x4858)
        if bytes_read >= 1026 {
            if (buffer[1024] == 0x48 && buffer[1025] == 0x2B) ||
               (buffer[1024] == 0x48 && buffer[1025] == 0x58) {
                disc_log!(info, "Found HFS+ signature (H+ or HX) at byte 1024");
                return Ok(DiscStructure::HfsDirectly);
            }
        }

        // Check for ISO9660 PVD at sector 16 (byte 32768)
        // PVD starts with 0x01 followed by "CD001"
        if bytes_read >= 32774 && 
           buffer[32768] == 0x01 &&
           &buffer[32769..32774] == b"CD001" {
            disc_log!(info, "Found ISO9660 PVD signature at byte 32768");
            return Ok(DiscStructure::Iso9660);
        } else if bytes_read >= 32774 {
            disc_log!(info, "Byte 32768-32773: {:02X?} (not ISO9660 PVD)", &buffer[32768..32774]);
        }

        disc_log!(warn, "No recognized disc structure found in first {}KB", bytes_read / 1024);
        Ok(DiscStructure::Unknown)
    }

    /// Try to detect HFS or HFS+ filesystem and extract volume name
    fn try_hfs_detection<R: Read + Seek>(reader: &mut R) -> Result<(
        FilesystemType,
        Option<String>,
        Option<PrimaryVolumeDescriptor>,
        Option<super::hfs::MasterDirectoryBlock>,
        Option<super::hfsplus::HfsPlusVolumeHeader>
    ), String> {
        disc_log!(info, "Starting HFS detection");
        
        // First, check for Apple Partition Map
        let partition_offset = match super::apm::find_hfs_partition_offset(reader) {
            Ok(offset) => {
                disc_log!(info, "Found HFS partition at offset: {} (block {})", offset, offset / 512);
                offset
            }
            Err(e) => {
                // No partition map, try direct HFS detection at byte 1024
                disc_log!(info, "No Apple Partition Map found ({}), trying direct HFS detection", e);
                0
            }
        };

        // HFS/HFS+ headers are at byte 1024 within the partition
        let header_offset = partition_offset + 1024;
        disc_log!(info, "Seeking to offset {} (partition {} + 1024) to read HFS headers", 
            header_offset, partition_offset);

        // Try HFS+ first (more common on newer Mac discs)
        disc_log!(info, "Attempting to parse HFS+ header...");
        reader.seek(SeekFrom::Start(header_offset))
            .map_err(|e| format!("Failed to seek to HFS+ header: {}", e))?;
        
        if let Ok((header, volume_name)) = super::hfsplus::HfsPlusVolumeHeader::parse_from_current_position(reader) {
            disc_log!(info, "HFS+ header parsed, signature: 0x{:04X}, valid: {}", header.signature, header.is_valid());
            if header.is_valid() {
                disc_log!(info, "Detected HFS+ volume: {}", volume_name);
                return Ok((FilesystemType::HfsPlus, Some(volume_name), None, None, Some(header)));
            } else {
                disc_log!(warn, "HFS+ header signature found but validation failed");
            }
        } else {
            disc_log!(info, "Failed to parse HFS+ header");
        }

        // Try HFS (classic)
        disc_log!(info, "Attempting to parse HFS (classic) header...");
        reader.seek(SeekFrom::Start(header_offset))
            .map_err(|e| format!("Failed to seek to HFS header: {}", e))?;
        
        if let Ok(mdb) = super::hfs::MasterDirectoryBlock::parse_from_current_position(reader) {
            disc_log!(info, "HFS MDB parsed, signature: 0x{:04X}, valid: {}, name: {}", 
                mdb.signature, mdb.is_valid(), mdb.volume_name);
            if mdb.is_valid() {
                disc_log!(info, "Detected HFS volume: {}", mdb.volume_name);
                return Ok((FilesystemType::Hfs, Some(mdb.volume_name.clone()), None, Some(mdb), None));
            } else {
                disc_log!(warn, "HFS MDB signature found but validation failed");
            }
        } else {
            disc_log!(info, "Failed to parse HFS (classic) header");
        }

        disc_log!(warn, "No valid HFS/HFS+ filesystem found at partition offset");
        Err("No HFS/HFS+ filesystem detected".to_string())
    }

    /// Read a CHD file
    fn read_chd(path: &Path, parsed_filename: ParsedFilename) -> Result<DiscInfo, DiscError> {
        match super::chd::read_chd(path) {
            Ok(chd_info) => {
                // Determine title and confidence
                let (title, confidence) = if let Some(ref label) = chd_info.volume_label {
                    if label.len() > 2 && !label.chars().all(|c| c.is_ascii_digit()) {
                        (super::identifier::normalize_volume_label(label), ConfidenceLevel::High)
                    } else {
                        (parsed_filename.title.clone(), ConfidenceLevel::Low)
                    }
                } else if let Some(ref meta) = chd_info.metadata {
                    // CHD metadata available but no volume label
                    (meta.clone(), ConfidenceLevel::Medium)
                } else {
                    (parsed_filename.title.clone(), ConfidenceLevel::Low)
                };

                Ok(DiscInfo {
                    path: path.to_path_buf(),
                    format: DiscFormat::Chd,
                    filesystem: chd_info.filesystem,
                    volume_label: chd_info.volume_label,
                    parsed_filename,
                    title,
                    confidence,
                    pvd: chd_info.pvd,
                    toc: chd_info.toc,
                    hfs_mdb: chd_info.hfs_mdb,
                    hfsplus_header: chd_info.hfsplus_header,
                })
            }
            Err(e) => {
                // Log the error but fall back to filename parsing
                log::warn!("Failed to read CHD file, falling back to filename: {}", e);

                Ok(DiscInfo {
                    path: path.to_path_buf(),
                    format: DiscFormat::Chd,
                    filesystem: FilesystemType::Unknown,
                    volume_label: None,
                    title: parsed_filename.title.clone(),
                    parsed_filename,
                    confidence: ConfidenceLevel::Low,
                    pvd: None,
                    toc: None,
                    hfs_mdb: None,
                    hfsplus_header: None,
                })
            }
        }
    }

    /// Read a BIN/CUE file
    fn read_bin_cue(path: &Path, parsed_filename: ParsedFilename) -> Result<DiscInfo, DiscError> {
        // For CUE files, read the CUE sheet
        // For BIN files, try to find the corresponding CUE file
        let cue_path = if path.extension().map(|e| e.to_ascii_lowercase()) == Some("cue".into()) {
            path.to_path_buf()
        } else {
            // It's a BIN file, look for matching CUE
            let cue_path = path.with_extension("cue");
            if cue_path.exists() {
                cue_path
            } else {
                // Try uppercase
                let cue_path_upper = path.with_extension("CUE");
                if cue_path_upper.exists() {
                    cue_path_upper
                } else {
                    // No CUE file found, fall back to filename parsing
                    log::warn!("No CUE file found for BIN, using filename parsing");
                    return Ok(DiscInfo {
                        path: path.to_path_buf(),
                        format: DiscFormat::BinCue,
                        filesystem: FilesystemType::Unknown,
                        volume_label: None,
                        title: parsed_filename.title.clone(),
                        parsed_filename,
                        confidence: ConfidenceLevel::Low,
                        pvd: None,
                        toc: None,
                        hfs_mdb: None,
                        hfsplus_header: None,
                    });
                }
            }
        };

        match super::bincue::read_bincue(&cue_path) {
            Ok(bincue_info) => {
                let (title, confidence) = if let Some(ref label) = bincue_info.volume_label {
                    if label.len() > 2 && !label.chars().all(|c| c.is_ascii_digit()) {
                        (super::identifier::normalize_volume_label(label), ConfidenceLevel::High)
                    } else {
                        (parsed_filename.title.clone(), ConfidenceLevel::Low)
                    }
                } else {
                    (parsed_filename.title.clone(), ConfidenceLevel::Low)
                };

                Ok(DiscInfo {
                    path: path.to_path_buf(),
                    format: DiscFormat::BinCue,
                    filesystem: bincue_info.filesystem,
                    volume_label: bincue_info.volume_label,
                    parsed_filename,
                    title,
                    confidence,
                    pvd: bincue_info.pvd,
                    toc: bincue_info.toc,
                    hfs_mdb: bincue_info.hfs_mdb,
                    hfsplus_header: bincue_info.hfsplus_header,
                })
            }
            Err(e) => {
                log::warn!("Failed to read BIN/CUE, falling back to filename: {}", e);
                Ok(DiscInfo {
                    path: path.to_path_buf(),
                    format: DiscFormat::BinCue,
                    filesystem: FilesystemType::Unknown,
                    volume_label: None,
                    title: parsed_filename.title.clone(),
                    parsed_filename,
                    confidence: ConfidenceLevel::Low,
                    pvd: None,
                    toc: None,
                    hfs_mdb: None,
                    hfsplus_header: None,
                })
            }
        }
    }

    /// Read an MDS/MDF file
    ///
    /// MDS/MDF support will be implemented in Phase 3.
    fn read_mds_mdf(path: &Path, parsed_filename: ParsedFilename) -> Result<DiscInfo, DiscError> {
        // TODO: Implement MDS/MDF reading
        log::warn!("MDS/MDF reading not yet implemented, using filename parsing");

        Ok(DiscInfo {
            path: path.to_path_buf(),
            format: DiscFormat::MdsMdf,
            filesystem: FilesystemType::Unknown,
            volume_label: None,
            title: parsed_filename.title.clone(),
            parsed_filename,
            confidence: ConfidenceLevel::Low,
            pvd: None,
            toc: None,
            hfs_mdb: None,
            hfsplus_header: None,
        })
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

        file.write_all(&pvd).unwrap();
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
