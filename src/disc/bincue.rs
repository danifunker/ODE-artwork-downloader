//! BIN/CUE disc image reading
//!
//! Parses CUE sheet files to find data tracks and reads ISO9660 volume information
//! from the corresponding BIN file.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use cue_sheet::parser::{parse_cue, Command, TrackType};

use super::iso9660::{PrimaryVolumeDescriptor, SECTOR_SIZE, PVD_SECTOR};

/// CD sector size for raw data (2352 bytes)
const CD_SECTOR_SIZE_RAW: u64 = 2352;

/// CD sector size for cooked data (2048 bytes)
const CD_SECTOR_SIZE_COOKED: u64 = 2048;

/// Offset to user data in a raw Mode 1 sector
const MODE1_DATA_OFFSET: u64 = 16;

/// Result type for BIN/CUE operations
pub type BinCueResult<T> = Result<T, BinCueError>;

/// Errors specific to BIN/CUE reading
#[derive(Debug)]
pub enum BinCueError {
    /// IO error
    Io(std::io::Error),
    /// CUE parsing error
    CueParse(String),
    /// BIN file not found
    BinNotFound(String),
    /// No data track found
    NoDataTrack,
    /// ISO9660 parsing error
    Iso9660(String),
}

impl std::fmt::Display for BinCueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::CueParse(s) => write!(f, "CUE parse error: {}", s),
            Self::BinNotFound(s) => write!(f, "BIN file not found: {}", s),
            Self::NoDataTrack => write!(f, "No data track found in CUE sheet"),
            Self::Iso9660(s) => write!(f, "ISO9660 error: {}", s),
        }
    }
}

impl std::error::Error for BinCueError {}

impl From<std::io::Error> for BinCueError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Information extracted from a BIN/CUE disc image
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BinCueInfo {
    /// Volume label from ISO9660 PVD (if found)
    pub volume_label: Option<String>,
    /// Full PVD (if found)
    pub pvd: Option<PrimaryVolumeDescriptor>,
    /// Path to the BIN file
    pub bin_path: std::path::PathBuf,
    /// Number of tracks found
    pub track_count: usize,
}

/// Track info extracted from CUE commands
#[derive(Debug)]
struct ParsedTrack {
    track_no: u32,
    track_type: TrackType,
    sector_size: u64,
    data_offset: u64,
    is_data: bool,
}

/// Read BIN/CUE disc image and extract information
///
/// # Arguments
/// * `cue_path` - Path to the CUE file (will look for BIN file referenced inside)
pub fn read_bincue(cue_path: &Path) -> BinCueResult<BinCueInfo> {
    // Read and parse the CUE file
    let cue_content = std::fs::read_to_string(cue_path)?;
    let commands = parse_cue(&cue_content)
        .map_err(|e| BinCueError::CueParse(format!("{:?}", e)))?;

    // Find the FILE command to get the BIN filename
    let mut bin_filename: Option<String> = None;
    let mut tracks: Vec<ParsedTrack> = Vec::new();

    for cmd in &commands {
        match cmd {
            Command::File(filename, _format) => {
                if bin_filename.is_none() {
                    bin_filename = Some(filename.clone());
                }
            }
            Command::Track(track_no, track_type) => {
                let (sector_size, data_offset, is_data) = get_track_params(track_type);
                tracks.push(ParsedTrack {
                    track_no: *track_no,
                    track_type: track_type.clone(),
                    sector_size,
                    data_offset,
                    is_data,
                });
                log::debug!("Track {}: {:?}, sector_size={}, is_data={}",
                    track_no, track_type, sector_size, is_data);
            }
            _ => {}
        }
    }

    let bin_filename = bin_filename
        .ok_or_else(|| BinCueError::CueParse("No FILE entry in CUE sheet".to_string()))?;

    log::debug!("Found {} tracks, BIN file: {}", tracks.len(), bin_filename);

    // Resolve BIN path relative to CUE file location
    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));
    let bin_path = resolve_bin_path(cue_dir, &bin_filename)?;

    // Open the BIN file and try to read PVD
    let file = File::open(&bin_path)?;
    let mut reader = BufReader::new(file);

    // Find first data track
    let data_track = tracks.iter().find(|t| t.is_data);

    let (volume_label, pvd) = if let Some(track) = data_track {
        match read_pvd_from_bin(&mut reader, track.sector_size, track.data_offset) {
            Ok(pvd) => {
                let label = if pvd.volume_id.is_empty() {
                    None
                } else {
                    Some(pvd.volume_id.clone())
                };
                (label, Some(pvd))
            }
            Err(e) => {
                log::warn!("Failed to read PVD with track params: {}", e);
                // Try fallback
                try_read_pvd_fallback(&mut reader)
            }
        }
    } else {
        log::warn!("No data track found, trying fallback");
        try_read_pvd_fallback(&mut reader)
    };

    Ok(BinCueInfo {
        volume_label,
        pvd,
        bin_path,
        track_count: tracks.len(),
    })
}

/// Get sector size and data offset for a track type
fn get_track_params(track_type: &TrackType) -> (u64, u64, bool) {
    match track_type {
        TrackType::Audio => (CD_SECTOR_SIZE_RAW, 0, false),
        TrackType::Cdg => (2448, 0, false),
        TrackType::Mode(mode, size) => {
            let is_data = *mode == 1 || *mode == 2;
            let data_offset = if *size == 2352 {
                if *mode == 1 { MODE1_DATA_OFFSET } else { 24 } // Mode 2 XA
            } else {
                0
            };
            (*size as u64, data_offset, is_data)
        }
        TrackType::Cdi(size) => (*size as u64, 8, true),
    }
}

/// Resolve BIN file path, trying different locations
fn resolve_bin_path(cue_dir: &Path, bin_filename: &str) -> BinCueResult<std::path::PathBuf> {
    // Try the path as-is (relative to CUE dir)
    let bin_path = cue_dir.join(bin_filename);
    if bin_path.exists() {
        return Ok(bin_path);
    }

    // Try just the filename (in case the CUE has an absolute path)
    if let Some(filename) = Path::new(bin_filename).file_name() {
        let bin_path = cue_dir.join(filename);
        if bin_path.exists() {
            return Ok(bin_path);
        }
    }

    // Try common variations
    let base = Path::new(bin_filename).file_stem().unwrap_or_default();
    for ext in &["bin", "BIN", "img", "IMG"] {
        let try_path = cue_dir.join(format!("{}.{}", base.to_string_lossy(), ext));
        if try_path.exists() {
            return Ok(try_path);
        }
    }

    Err(BinCueError::BinNotFound(bin_path.display().to_string()))
}

/// Read PVD from BIN file
fn read_pvd_from_bin<R: Read + Seek>(
    reader: &mut R,
    sector_size: u64,
    data_offset: u64,
) -> BinCueResult<PrimaryVolumeDescriptor> {
    // Calculate byte offset to PVD (sector 16)
    let pvd_byte_offset = (PVD_SECTOR * sector_size) + data_offset;

    log::debug!(
        "Reading PVD: sector_size={}, data_offset={}, pvd_byte_offset={}",
        sector_size, data_offset, pvd_byte_offset
    );

    reader.seek(SeekFrom::Start(pvd_byte_offset))?;

    let mut buffer = [0u8; SECTOR_SIZE as usize];
    reader.read_exact(&mut buffer)?;

    PrimaryVolumeDescriptor::parse(&buffer).map_err(BinCueError::Iso9660)
}

/// Try different sector formats as fallback
fn try_read_pvd_fallback<R: Read + Seek>(reader: &mut R) -> (Option<String>, Option<PrimaryVolumeDescriptor>) {
    let formats = [
        (CD_SECTOR_SIZE_COOKED, 0u64),       // Cooked 2048
        (CD_SECTOR_SIZE_RAW, MODE1_DATA_OFFSET), // Raw Mode 1
        (CD_SECTOR_SIZE_RAW, 24),            // Raw Mode 2 XA
    ];

    for (sector_size, data_offset) in formats {
        if let Ok(pvd) = read_pvd_from_bin(reader, sector_size, data_offset) {
            log::debug!("Found PVD with fallback: sector_size={}, data_offset={}",
                sector_size, data_offset);
            let label = if pvd.volume_id.is_empty() {
                None
            } else {
                Some(pvd.volume_id.clone())
            };
            return (label, Some(pvd));
        }
    }

    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_track_params() {
        // Audio track
        let (size, offset, is_data) = get_track_params(&TrackType::Audio);
        assert_eq!(size, 2352);
        assert_eq!(offset, 0);
        assert!(!is_data);

        // Mode 1 cooked (2048)
        let (size, offset, is_data) = get_track_params(&TrackType::Mode(1, 2048));
        assert_eq!(size, 2048);
        assert_eq!(offset, 0);
        assert!(is_data);

        // Mode 1 raw (2352)
        let (size, offset, is_data) = get_track_params(&TrackType::Mode(1, 2352));
        assert_eq!(size, 2352);
        assert_eq!(offset, 16);
        assert!(is_data);
    }
}
