//! Disc image reader
//!
//! Unified interface for reading disc images in various formats.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use thiserror::Error;

use super::formats::{DiscFormat, FilesystemType};
use super::identifier::{parse_filename, ConfidenceLevel, ParsedFilename};
use super::iso9660::PrimaryVolumeDescriptor;

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

        let pvd = PrimaryVolumeDescriptor::read_from(&mut reader)
            .map_err(DiscError::ParseError)?;

        let volume_label = if pvd.volume_id.is_empty() {
            None
        } else {
            Some(pvd.volume_id.clone())
        };

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
            filesystem: FilesystemType::Iso9660,
            volume_label,
            parsed_filename,
            title,
            confidence,
            pvd: Some(pvd),
        })
    }

    /// Read a CHD file
    fn read_chd(path: &Path, parsed_filename: ParsedFilename) -> Result<DiscInfo, DiscError> {
        match super::chd::read_chd(path) {
            Ok(chd_info) => {
                // Determine filesystem type based on whether we found ISO9660 data
                let filesystem = if chd_info.pvd.is_some() {
                    FilesystemType::Iso9660
                } else {
                    FilesystemType::Unknown
                };

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
                    filesystem,
                    volume_label: chd_info.volume_label,
                    parsed_filename,
                    title,
                    confidence,
                    pvd: chd_info.pvd,
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
                    });
                }
            }
        };

        match super::bincue::read_bincue(&cue_path) {
            Ok(bincue_info) => {
                let filesystem = if bincue_info.pvd.is_some() {
                    FilesystemType::Iso9660
                } else {
                    FilesystemType::Unknown
                };

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
                    filesystem,
                    volume_label: bincue_info.volume_label,
                    parsed_filename,
                    title,
                    confidence,
                    pvd: bincue_info.pvd,
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
