//! BIN/CUE disc image reading
//!
//! Parses CUE sheet files to find data tracks and reads ISO9660 volume information
//! from the corresponding BIN file.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use cue_sheet::parser::{parse_cue, Command, TrackType};

use super::iso9660::{PrimaryVolumeDescriptor, SECTOR_SIZE, PVD_SECTOR};
use super::toc::{DiscTOC, TrackInfo, parse_msf};

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
#[allow(dead_code)]
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
    /// Table of Contents (for audio CDs)
    pub toc: Option<DiscTOC>,
    /// HFS Master Directory Block (if found)
    pub hfs_mdb: Option<super::hfs::MasterDirectoryBlock>,
    /// HFS+ Volume Header (if found)
    pub hfsplus_header: Option<super::hfsplus::HfsPlusVolumeHeader>,
    /// Filesystem type detected
    pub filesystem: super::formats::FilesystemType,
}

/// Track info extracted from CUE commands
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ParsedTrack {
    track_no: u32,
    track_type: TrackType,
    sector_size: u64,
    data_offset: u64,
    is_data: bool,
    bin_filename: String,  // The BIN file this track belongs to
    index_01: Option<String>,  // INDEX 01 position (MM:SS:FF)
}

/// Normalize CUE file for parser compatibility
/// - Fixes case-sensitivity issues (BINARY -> Binary)
/// - Removes/fixes problematic lines (CATALOG with leading zeros)
fn normalize_cue_keywords(content: &str) -> String {
    let mut result = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip CATALOG lines (parser has issues with number parsing)
        // CATALOG is optional and not needed for reading disc data
        if trimmed.starts_with("CATALOG") {
            continue;
        }

        // Skip REM (comment) lines that might cause issues
        if trimmed.starts_with("REM ") {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    // Fix case-sensitive file format keywords
    result
        .replace("BINARY", "Binary")
        .replace("MOTOROLA", "Motorola")
        .replace(" WAVE", " Wave")  // Be careful not to replace in filenames
        .replace(" MP3", " Mp3")
        .replace(" AIFF", " Aiff")
}

/// Detect filesystem type from BIN file
/// Returns (volume_label, pvd, hfs_mdb, hfsplus_header, filesystem_type)
fn detect_filesystem<R: Read + Seek>(
    reader: &mut R,
    sector_size: u64,
    data_offset: u64,
    track_type: &TrackType,
) -> (
    Option<String>,
    Option<PrimaryVolumeDescriptor>,
    Option<super::hfs::MasterDirectoryBlock>,
    Option<super::hfsplus::HfsPlusVolumeHeader>,
    super::formats::FilesystemType,
) {
    use super::formats::FilesystemType;

    // Try ISO9660 first
    match read_pvd_from_bin(reader, sector_size, data_offset) {
        Ok(pvd) => {
            log::debug!("ISO9660 PVD parsed successfully, volume_id: '{}'", pvd.volume_id);
            let label = if pvd.volume_id.is_empty() {
                None
            } else {
                Some(pvd.volume_id.clone())
            };
            return (label, Some(pvd), None, None, FilesystemType::Iso9660);
        }
        Err(e) => {
            log::debug!("ISO9660 PVD read failed: {}, trying HFS/HFS+", e);
        }
    }

    // Try HFS/HFS+ detection
    // HFS headers are at logical byte 1024 from the start of the data
    // We need to calculate the physical offset accounting for sector format
    match read_hfs_headers_from_bin(reader, sector_size, data_offset, track_type) {
        Ok((mdb, header, label, fs_type)) => {
            log::debug!("Detected HFS/HFS+ filesystem: {:?}, label: {:?}", fs_type, label);
            return (label, None, mdb, header, fs_type);
        }
        Err(e) => {
            log::debug!("HFS/HFS+ detection failed: {}", e);
        }
    }

    // No filesystem detected, try fallback PVD read
    match try_read_pvd_fallback(reader) {
        (Some(label), Some(pvd)) => {
            (Some(label), Some(pvd), None, None, FilesystemType::Iso9660)
        }
        _ => {
            log::warn!("No filesystem detected");
            (None, None, None, None, FilesystemType::Unknown)
        }
    }
}

/// Get logical data sector size for a data track
fn get_logical_sector_size(track_type: &TrackType) -> u64 {
    match track_type {
        TrackType::Mode(1, _) => CD_SECTOR_SIZE_COOKED, // Mode 1 is always 2048
        TrackType::Mode(2, 2352) => 2336, // Mode 2/XA raw has 2336 bytes of data
        TrackType::Mode(2, size) => *size as u64, // Cooked Mode 2
        TrackType::Cdi(size) => *size as u64,
        _ => CD_SECTOR_SIZE_COOKED, // Fallback for other data track types
    }
}

/// Read HFS/HFS+ headers from BIN file at logical offset 1024
fn read_hfs_headers_from_bin<R: Read + Seek>(
    reader: &mut R,
    sector_size: u64,
    data_offset: u64,
    track_type: &TrackType,
) -> Result<(
    Option<super::hfs::MasterDirectoryBlock>,
    Option<super::hfsplus::HfsPlusVolumeHeader>,
    Option<String>,
    super::formats::FilesystemType,
), String> {
    use super::formats::FilesystemType;

    // HFS headers are at logical byte 1024
    // Calculate which sector contains byte 1024
    let logical_offset = 1024u64;
    
    // For sector-based formats, we need to read the sector containing byte 1024
    // and extract the header from within that sector
    let logical_sector_size = get_logical_sector_size(track_type);
    let sector_number = logical_offset / logical_sector_size;
    let offset_in_sector = logical_offset % logical_sector_size;
    
    // Calculate physical byte offset in the BIN file
    let physical_offset = (sector_number * sector_size) + data_offset + offset_in_sector;
    
    log::debug!("Reading HFS headers: logical_offset={}, sector={}, offset_in_sector={}, physical_offset={}", 
        logical_offset, sector_number, offset_in_sector, physical_offset);
    
    // Read enough data for HFS+ header (512 bytes)
    reader.seek(SeekFrom::Start(physical_offset))
        .map_err(|e| format!("Failed to seek to HFS header: {}", e))?;
    
    let mut buffer = vec![0u8; 512];
    reader.read_exact(&mut buffer)
        .map_err(|e| format!("Failed to read HFS header: {}", e))?;
    
    // Try HFS+ first
    let mut cursor = std::io::Cursor::new(&buffer);
    if let Ok((header, volume_name)) = super::hfsplus::HfsPlusVolumeHeader::parse_from_current_position(&mut cursor) {
        if header.is_valid() {
            log::debug!("Detected HFS+ volume: {}", volume_name);
            return Ok((None, Some(header), Some(volume_name), FilesystemType::HfsPlus));
        }
    }
    
    // Try HFS classic (needs 162 bytes but we have 512)
    let mut cursor = std::io::Cursor::new(&buffer);
    if let Ok(mdb) = super::hfs::MasterDirectoryBlock::parse_from_current_position(&mut cursor) {
        if mdb.is_valid() {
            log::debug!("Detected HFS volume: {}", mdb.volume_name);
            return Ok((Some(mdb.clone()), None, Some(mdb.volume_name), FilesystemType::Hfs));
        }
    }
    
    Err("No valid HFS/HFS+ filesystem found".to_string())
}

/// Read BIN/CUE disc image and extract information
///
/// # Arguments
/// * `cue_path` - Path to the CUE file (will look for BIN file referenced inside)
pub fn read_bincue(cue_path: &Path) -> BinCueResult<BinCueInfo> {
    // Read and parse the CUE file
    let cue_content = std::fs::read_to_string(cue_path)?;

    // Normalize file format keywords (cue_sheet crate is case-sensitive)
    // CUE files often have "BINARY" but crate expects "Binary"
    let cue_content = normalize_cue_keywords(&cue_content);

    let commands = parse_cue(&cue_content)
        .map_err(|e| BinCueError::CueParse(format!("{:?}", e)))?;

    // Parse commands, tracking current BIN file for each track
    let mut current_bin_filename: Option<String> = None;
    let mut tracks: Vec<ParsedTrack> = Vec::new();
    let mut current_track_index: Option<usize> = None;

    for cmd in &commands {
        match cmd {
            Command::File(filename, _format) => {
                current_bin_filename = Some(filename.clone());
            }
            Command::Track(track_no, track_type) => {
                let (sector_size, data_offset, is_data) = get_track_params(track_type);
                let bin_filename = current_bin_filename.clone()
                    .unwrap_or_else(|| "unknown.bin".to_string());
                tracks.push(ParsedTrack {
                    track_no: *track_no,
                    track_type: track_type.clone(),
                    sector_size,
                    data_offset,
                    is_data,
                    bin_filename: bin_filename.clone(),
                    index_01: None,
                });
                current_track_index = Some(tracks.len() - 1);
                log::debug!("Track {}: {:?}, sector_size={}, is_data={}, file={}",
                    track_no, track_type, sector_size, is_data, bin_filename);
            }
            Command::Index(index_no, msf) => {
                // Capture INDEX 01 position for TOC calculation
                if *index_no == 1 {
                    if let Some(idx) = current_track_index {
                        if let Some(track) = tracks.get_mut(idx) {
                            track.index_01 = Some(format!("{}:{}:{}", msf.mins, msf.secs, msf.frames));
                            log::debug!("Track {} INDEX 01: {}:{}:{}", track.track_no, msf.mins, msf.secs, msf.frames);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if tracks.is_empty() {
        return Err(BinCueError::CueParse("No TRACK entries in CUE sheet".to_string()));
    }

    log::debug!("Found {} tracks", tracks.len());

    // Find first data track
    let data_track = tracks.iter().find(|t| t.is_data);

    // Resolve CUE directory for finding BIN files
    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));

    // For return value, use the first data track's BIN file, or first track's
    let primary_bin_filename = data_track
        .map(|t| &t.bin_filename)
        .or_else(|| tracks.first().map(|t| &t.bin_filename))
        .ok_or_else(|| BinCueError::CueParse("No tracks found".to_string()))?;

    let bin_path = resolve_bin_path_with_cue_fallback(cue_dir, primary_bin_filename, Some(cue_path))?;

    // Try to read PVD from the data track's BIN file
    let (volume_label, pvd, hfs_mdb, hfsplus_header, filesystem) = if let Some(track) = data_track {
        // Open the specific BIN file for this track
        let track_bin_path = resolve_bin_path_with_cue_fallback(cue_dir, &track.bin_filename, Some(cue_path))?;
        log::debug!("Opening data track BIN file: {}", track_bin_path.display());
        let file = File::open(&track_bin_path)?;
        let mut reader = BufReader::new(file);

        // Try to detect filesystem type
        detect_filesystem(&mut reader, track.sector_size, track.data_offset, &track.track_type)
    } else {
        log::warn!("No data track found, trying fallback on first BIN file");
        let file = File::open(&bin_path)?;
        let mut reader = BufReader::new(file);
        // Fallback with default track type assumption
        let fallback_track_type = TrackType::Mode(1, 2048);
        detect_filesystem(&mut reader, CD_SECTOR_SIZE_COOKED, 0, &fallback_track_type)
    };

    // Extract TOC if we have audio tracks
    let toc = extract_toc(&tracks, &bin_path);
    if let Some(ref toc) = toc {
        log::debug!("TOC extracted: {} tracks, MusicBrainz ID: {}", 
            toc.track_count(), toc.calculate_musicbrainz_id());
    }

    Ok(BinCueInfo {
        volume_label,
        pvd,
        bin_path,
        track_count: tracks.len(),
        toc,
        hfs_mdb,
        hfsplus_header,
        filesystem,
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
/// Falls back to CUE basename if the referenced BIN file doesn't exist
fn resolve_bin_path_with_cue_fallback(
    cue_dir: &Path,
    bin_filename: &str,
    cue_path: Option<&Path>,
) -> BinCueResult<std::path::PathBuf> {
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

    // Try common variations of the referenced filename
    let base = Path::new(bin_filename).file_stem().unwrap_or_default();
    for ext in &["bin", "BIN", "img", "IMG"] {
        let try_path = cue_dir.join(format!("{}.{}", base.to_string_lossy(), ext));
        if try_path.exists() {
            return Ok(try_path);
        }
    }

    // If CUE path provided, try using the CUE's basename with BIN extensions
    // This handles cases where the BIN was renamed to match the CUE
    if let Some(cue) = cue_path {
        if let Some(cue_stem) = cue.file_stem() {
            log::debug!("BIN not found, trying CUE basename: {}", cue_stem.to_string_lossy());
            for ext in &["bin", "BIN", "img", "IMG"] {
                let try_path = cue_dir.join(format!("{}.{}", cue_stem.to_string_lossy(), ext));
                if try_path.exists() {
                    log::debug!("Found BIN using CUE basename: {}", try_path.display());
                    return Ok(try_path);
                }
            }
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
    // For raw sectors (2352), data_offset is where user data starts within the sector
    let pvd_byte_offset = (PVD_SECTOR * sector_size) + data_offset;

    log::debug!(
        "Reading PVD: sector_size={}, data_offset={}, pvd_byte_offset={}",
        sector_size, data_offset, pvd_byte_offset
    );

    reader.seek(SeekFrom::Start(pvd_byte_offset))?;

    let mut buffer = [0u8; SECTOR_SIZE as usize];
    reader.read_exact(&mut buffer)?;

    // Debug: show first few bytes to verify we're reading the right data
    log::debug!(
        "PVD first 8 bytes: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
        buffer[0], buffer[1], buffer[2], buffer[3],
        buffer[4], buffer[5], buffer[6], buffer[7]
    );

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

/// Extract TOC information from parsed tracks
fn extract_toc(tracks: &[ParsedTrack], bin_path: &Path) -> Option<DiscTOC> {
    // Only extract TOC if we have tracks with INDEX 01 information
    let tracks_with_index: Vec<_> = tracks.iter()
        .filter(|t| t.index_01.is_some())
        .collect();

    if tracks_with_index.is_empty() {
        log::debug!("No tracks with INDEX 01 found, skipping TOC extraction");
        return None;
    }

    // Check if any tracks are audio
    let has_audio = tracks.iter().any(|t| matches!(t.track_type, TrackType::Audio));
    if !has_audio {
        log::debug!("No audio tracks found (all data tracks), skipping TOC extraction");
        return None;
    }

    // Convert to TrackInfo
    let mut track_infos = Vec::new();
    for track in tracks_with_index {
        if let Some(ref msf) = track.index_01 {
            if let Some(offset) = parse_msf(msf) {
                let track_type = match track.track_type {
                    TrackType::Audio => "AUDIO",
                    TrackType::Mode(1, _) => "MODE1",
                    TrackType::Mode(2, _) => "MODE2",
                    _ => "OTHER",
                };
                
                track_infos.push(TrackInfo {
                    number: track.track_no as u8,
                    offset,
                    track_type: track_type.to_string(),
                });
            }
        }
    }

    if track_infos.is_empty() {
        return None;
    }

    log::info!("Extracting TOC for {} tracks ({} audio)", 
        track_infos.len(), 
        track_infos.iter().filter(|t| t.track_type == "AUDIO").count());

    // Calculate total length from BIN file size
    let total_length_frames = if let Ok(metadata) = std::fs::metadata(bin_path) {
        let file_size = metadata.len();
        // Assume first track's sector size for calculation
        let sector_size = tracks.first().map(|t| t.sector_size).unwrap_or(2352);
        let total_sectors = file_size / sector_size;
        total_sectors as u32 // Convert sectors to frames (1:1 for CD)
    } else {
        // Fallback: use last track offset + estimated track length
        track_infos.last().map(|t| t.offset + 5000).unwrap_or(0)
    };

    DiscTOC::from_tracks(track_infos, total_length_frames)
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

    #[test]
    fn test_normalize_cue_keywords() {
        let input = r#"FILE "game.bin" BINARY
TRACK 01 MODE1/2352
  INDEX 01 00:00:00"#;
        let output = normalize_cue_keywords(input);
        assert!(output.contains("Binary"));
        assert!(!output.contains("BINARY"));
    }

    #[test]
    fn test_parse_multifile_cue() {
        let cue_content = r#"CATALOG 0000000000000
FILE "Batman Forever (Europe) (Track 01).bin" BINARY
  TRACK 01 MODE1/2352
    INDEX 01 00:00:00
FILE "Batman Forever (Europe) (Track 02).bin" BINARY
  TRACK 02 AUDIO
    INDEX 00 00:00:00
    INDEX 01 00:02:00"#;

        let normalized = normalize_cue_keywords(cue_content);
        println!("Normalized:\n{}", normalized);

        let result = parse_cue(&normalized);
        println!("Parse result: {:?}", result);
        assert!(result.is_ok(), "Failed to parse CUE: {:?}", result.err());

        let commands = result.unwrap();
        assert!(commands.len() >= 4, "Expected at least 4 commands, got {}", commands.len());
    }
}
