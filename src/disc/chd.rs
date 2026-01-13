//! CHD (Compressed Hunks of Data) file reading
//!
//! Provides functionality to read CHD files and extract ISO9660 volume information.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use chd::Chd;
use chd::metadata::MetadataTag;

use super::iso9660::{PrimaryVolumeDescriptor, SECTOR_SIZE, PVD_SECTOR};
use super::toc::{DiscTOC, TrackInfo as TocTrackInfo};

/// CD sector size with full subchannel data (raw)
const CD_SECTOR_SIZE_RAW: u32 = 2352;

/// CD sector size for Mode 1/2 data (cooked)
const CD_SECTOR_SIZE_COOKED: u32 = 2048;

/// Offset to user data in a raw CD sector (Mode 1: 16 bytes sync+header)
const CD_MODE1_DATA_OFFSET: usize = 16;

/// CHT2 metadata tag (CD-ROM Track v2)
const CHT2_TAG: u32 = 0x43485432; // "CHT2"

/// Result type for CHD operations
pub type ChdResult<T> = Result<T, ChdError>;

/// Errors specific to CHD reading
#[derive(Debug)]
pub enum ChdError {
    /// IO error
    Io(std::io::Error),
    /// CHD parsing error
    Parse(String),
    /// Sector read error
    SectorRead(String),
    /// ISO9660 parsing error
    Iso9660(String),
}

impl std::fmt::Display for ChdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Parse(s) => write!(f, "CHD parse error: {}", s),
            Self::SectorRead(s) => write!(f, "Sector read error: {}", s),
            Self::Iso9660(s) => write!(f, "ISO9660 error: {}", s),
        }
    }
}

impl std::error::Error for ChdError {}

impl From<std::io::Error> for ChdError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Information extracted from a CHD file
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChdInfo {
    /// Volume label from ISO9660 PVD (if found)
    pub volume_label: Option<String>,
    /// Full PVD (if found)
    pub pvd: Option<PrimaryVolumeDescriptor>,
    /// CHD metadata (game title from CHD header if available)
    pub metadata: Option<String>,
    /// Hunk size
    pub hunk_size: u32,
    /// Total size of uncompressed data
    pub logical_size: u64,
    /// Table of Contents (for audio CDs)
    pub toc: Option<DiscTOC>,
}

/// Track information parsed from CHD metadata
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TrackInfo {
    track_num: u32,
    track_type: String,
    frames: u32,
    frame_offset: u32, // Cumulative offset in frames
}

/// Read CHD file and extract disc information
pub fn read_chd(path: &Path) -> ChdResult<ChdInfo> {
    let file = File::open(path)?;
    let mut buf_reader = BufReader::new(file);

    // Open the CHD file
    let mut chd = Chd::open(&mut buf_reader, None)
        .map_err(|e| ChdError::Parse(format!("Failed to open CHD: {:?}", e)))?;

    let header = chd.header();
    let hunk_size = header.hunk_size();
    let logical_size = header.logical_bytes();

    // Parse track metadata to find data tracks
    let tracks = parse_track_metadata(&mut chd)?;
    log::debug!("Found {} tracks in CHD", tracks.len());

    for track in &tracks {
        log::debug!(
            "Track {}: type={}, frames={}, offset={}",
            track.track_num, track.track_type, track.frames, track.frame_offset
        );
    }

    // Try to read the ISO9660 PVD from the disc data
    let (volume_label, pvd) = match read_pvd_from_chd(&mut chd, &tracks) {
        Ok(pvd) => {
            let label = if pvd.volume_id.is_empty() {
                None
            } else {
                Some(pvd.volume_id.clone())
            };
            (label, Some(pvd))
        }
        Err(e) => {
            log::warn!("Could not read ISO9660 PVD from CHD: {}", e);
            (None, None)
        }
    };

    // Extract TOC if we have audio tracks
    let toc = extract_toc_from_tracks(&tracks);
    if let Some(ref toc) = toc {
        log::debug!("TOC extracted from CHD: {} tracks, MusicBrainz ID: {}", 
            toc.track_count(), toc.calculate_musicbrainz_id());
    }

    Ok(ChdInfo {
        volume_label,
        pvd,
        metadata: None,
        hunk_size,
        logical_size,
        toc,
    })
}

/// Parse CHT2 track metadata from CHD
/// Collects refs first, then reads content to work around borrow checker
fn parse_track_metadata<F: Read + Seek>(chd: &mut Chd<F>) -> ChdResult<Vec<TrackInfo>> {
    // First pass: collect metadata refs (they're Clone)
    let meta_refs: Vec<_> = chd
        .metadata_refs()
        .filter(|meta_ref| meta_ref.metatag() == CHT2_TAG)
        .collect();

    // Second pass: read each metadata entry
    let mut tracks = Vec::new();
    let mut frame_offset = 0u32;

    for meta_ref in meta_refs {
        match meta_ref.read(chd.inner()) {
            Ok(metadata) => {
                if let Ok(content) = String::from_utf8(metadata.value.clone()) {
                    log::debug!("CHT2 metadata: {}", content);
                    if let Some(track) = parse_cht2_entry(&content, frame_offset) {
                        frame_offset += track.frames;
                        tracks.push(track);
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to read CHT2 metadata: {:?}", e);
            }
        }
    }

    // Sort by track number
    tracks.sort_by_key(|t| t.track_num);

    // Recalculate frame offsets after sorting
    let mut offset = 0u32;
    for track in &mut tracks {
        track.frame_offset = offset;
        offset += track.frames;
    }

    Ok(tracks)
}

/// Parse a single CHT2 metadata entry
fn parse_cht2_entry(content: &str, frame_offset: u32) -> Option<TrackInfo> {
    let mut track_num = 0u32;
    let mut track_type = String::new();
    let mut frames = 0u32;

    for part in content.split_whitespace() {
        if let Some((key, value)) = part.split_once(':') {
            match key {
                "TRACK" => track_num = value.parse().unwrap_or(0),
                "TYPE" => track_type = value.to_string(),
                "FRAMES" => frames = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }

    if track_num > 0 {
        Some(TrackInfo {
            track_num,
            track_type,
            frames,
            frame_offset,
        })
    } else {
        None
    }
}

/// Check if a track type is a data track (not audio)
fn is_data_track(track_type: &str) -> bool {
    track_type.starts_with("MODE1") || track_type.starts_with("MODE2")
}

/// Check if a track type is an audio track
fn is_audio_track(track_type: &str) -> bool {
    track_type == "AUDIO"
}

/// Extract TOC information from CHD track metadata
fn extract_toc_from_tracks(tracks: &[TrackInfo]) -> Option<DiscTOC> {
    // Filter for audio tracks only
    let audio_tracks: Vec<_> = tracks.iter()
        .filter(|t| is_audio_track(&t.track_type))
        .collect();

    if audio_tracks.is_empty() {
        return None;
    }

    // Convert to TOC TrackInfo format
    let toc_tracks: Vec<TocTrackInfo> = audio_tracks.iter()
        .map(|t| TocTrackInfo {
            number: t.track_num as u8,
            offset: t.frame_offset,
            track_type: t.track_type.clone(),
        })
        .collect();

    // Calculate total length from last track
    let total_length_frames = tracks.iter()
        .map(|t| t.frame_offset + t.frames)
        .max()
        .unwrap_or(0);

    DiscTOC::from_tracks(toc_tracks, total_length_frames)
}

/// Get sector size for track type
fn get_track_sector_size(track_type: &str) -> (u32, usize) {
    // CHD CD-ROM data uses raw frames (2352 bytes + 96 bytes subcode = 2448)
    // The actual sector size and data offset depend on the track type
    if track_type.contains("RAW") {
        // Raw mode: 2352 bytes per sector, data at offset 16 (after sync+header)
        (CD_SECTOR_SIZE_RAW, CD_MODE1_DATA_OFFSET)
    } else {
        // Cooked mode: 2048 bytes of user data
        (CD_SECTOR_SIZE_COOKED, 0)
    }
}

/// CD frame size in CHD (raw sector + subcode)
const CD_FRAME_SIZE: u32 = 2352 + 96; // 2448 bytes

/// Read the Primary Volume Descriptor from CHD disc data
fn read_pvd_from_chd<F: Read + Seek>(
    chd: &mut Chd<F>,
    tracks: &[TrackInfo],
) -> ChdResult<PrimaryVolumeDescriptor> {
    let header = chd.header();
    let hunk_size = header.hunk_size();

    // Find data tracks
    let data_tracks: Vec<_> = tracks
        .iter()
        .filter(|t| is_data_track(&t.track_type))
        .collect();

    log::debug!("Found {} data tracks", data_tracks.len());

    // Try each data track
    for track in &data_tracks {
        log::debug!(
            "Trying track {} (type: {}, frame_offset: {}, frames: {})",
            track.track_num, track.track_type, track.frame_offset, track.frames
        );

        let (sector_size, data_offset) = get_track_sector_size(&track.track_type);

        // In CHD, each frame is CD_FRAME_SIZE bytes (2448)
        // PVD is at sector 16 within the data track
        // Track data starts at frame_offset * CD_FRAME_SIZE
        let track_byte_offset = track.frame_offset as u64 * CD_FRAME_SIZE as u64;

        // For raw sectors, we need to account for the sync/header (16 bytes)
        // The PVD is at sector 16, so the byte offset is:
        // track_start + (16 * frame_size) + data_offset_in_frame
        let pvd_byte_offset = track_byte_offset + (PVD_SECTOR * CD_FRAME_SIZE as u64);

        log::debug!(
            "Track byte offset: {}, PVD byte offset: {}, sector_size: {}, data_offset: {}",
            track_byte_offset, pvd_byte_offset, sector_size, data_offset
        );

        match try_read_pvd_at_offset(chd, hunk_size, pvd_byte_offset, CD_FRAME_SIZE, data_offset) {
            Ok(pvd) => return Ok(pvd),
            Err(e) => {
                log::debug!("Failed to read PVD from track {}: {}", track.track_num, e);
            }
        }
    }

    // If no tracks found, try legacy approach
    if tracks.is_empty() {
        log::debug!("No track metadata found, trying legacy offsets");
        return try_legacy_pvd_read(chd, hunk_size);
    }

    Err(ChdError::Iso9660("Could not find valid ISO9660 PVD in any data track".to_string()))
}

/// Try to read PVD using legacy approach (for CHDs without track metadata)
fn try_legacy_pvd_read<F: Read + Seek>(
    chd: &mut Chd<F>,
    hunk_size: u32,
) -> ChdResult<PrimaryVolumeDescriptor> {
    // Try common sector sizes
    let attempts = [
        (CD_SECTOR_SIZE_COOKED, 0usize),
        (CD_SECTOR_SIZE_RAW, CD_MODE1_DATA_OFFSET),
    ];

    for (sector_size, data_offset) in attempts {
        let pvd_byte_offset = PVD_SECTOR * sector_size as u64;
        match try_read_pvd_at_offset(chd, hunk_size, pvd_byte_offset, sector_size, data_offset) {
            Ok(pvd) => return Ok(pvd),
            Err(e) => {
                log::debug!("Legacy read failed with sector_size={}: {}", sector_size, e);
            }
        }
    }

    Err(ChdError::Iso9660("Could not find valid ISO9660 PVD".to_string()))
}

/// Try to read PVD at a specific byte offset
fn try_read_pvd_at_offset<F: Read + Seek>(
    chd: &mut Chd<F>,
    hunk_size: u32,
    pvd_byte_offset: u64,
    sector_size: u32,
    data_offset_in_sector: usize,
) -> ChdResult<PrimaryVolumeDescriptor> {
    // Calculate which hunk contains this offset
    let hunk_index = (pvd_byte_offset / hunk_size as u64) as u32;
    let offset_in_hunk = (pvd_byte_offset % hunk_size as u64) as usize;

    log::debug!(
        "Reading PVD: byte_offset={}, hunk={}, offset_in_hunk={}, sector_size={}, data_offset={}",
        pvd_byte_offset, hunk_index, offset_in_hunk, sector_size, data_offset_in_sector
    );

    // Read the hunk containing the PVD
    let mut compressed_buf = Vec::new();
    let mut hunk_buf = chd.get_hunksized_buffer();
    chd.hunk(hunk_index)
        .map_err(|e| ChdError::SectorRead(format!("Failed to get hunk {}: {:?}", hunk_index, e)))?
        .read_hunk_in(&mut compressed_buf, &mut hunk_buf)
        .map_err(|e| ChdError::SectorRead(format!("Failed to read hunk: {:?}", e)))?;

    // Calculate where the actual data starts
    let data_start = offset_in_hunk + data_offset_in_sector;

    // Check if we have enough data in this hunk
    if data_start + SECTOR_SIZE as usize > hunk_buf.len() {
        return read_pvd_spanning_hunks(chd, hunk_size, pvd_byte_offset, sector_size, data_offset_in_sector);
    }

    let sector_data = &hunk_buf[data_start..data_start + SECTOR_SIZE as usize];

    PrimaryVolumeDescriptor::parse(sector_data).map_err(ChdError::Iso9660)
}

/// Handle case where PVD spans multiple hunks
fn read_pvd_spanning_hunks<F: Read + Seek>(
    chd: &mut Chd<F>,
    hunk_size: u32,
    pvd_offset: u64,
    sector_size: u32,
    data_offset_in_sector: usize,
) -> ChdResult<PrimaryVolumeDescriptor> {
    let hunk_size_u64 = hunk_size as u64;

    // Read enough hunks to cover the PVD sector
    let mut data = Vec::new();
    let start_hunk = (pvd_offset / hunk_size_u64) as u32;
    let end_offset = pvd_offset + sector_size as u64; // Full sector
    let end_hunk = ((end_offset + hunk_size_u64 - 1) / hunk_size_u64) as u32;

    let mut compressed_buf = Vec::new();
    let mut hunk_buf = chd.get_hunksized_buffer();

    for hunk_idx in start_hunk..=end_hunk {
        chd.hunk(hunk_idx)
            .map_err(|e| ChdError::SectorRead(format!("Failed to get hunk {}: {:?}", hunk_idx, e)))?
            .read_hunk_in(&mut compressed_buf, &mut hunk_buf)
            .map_err(|e| ChdError::SectorRead(format!("Failed to read hunk: {:?}", e)))?;
        data.extend_from_slice(&hunk_buf);
    }

    // Calculate offset within our combined buffer
    let buffer_offset = (pvd_offset - (start_hunk as u64 * hunk_size_u64)) as usize;
    let data_start = buffer_offset + data_offset_in_sector;

    if data_start + SECTOR_SIZE as usize > data.len() {
        return Err(ChdError::SectorRead("PVD sector out of bounds".to_string()));
    }

    let sector_data = &data[data_start..data_start + SECTOR_SIZE as usize];

    PrimaryVolumeDescriptor::parse(sector_data).map_err(ChdError::Iso9660)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sector_calculations() {
        assert_eq!(CD_SECTOR_SIZE_RAW, 2352);
        assert_eq!(CD_SECTOR_SIZE_COOKED, 2048);
        assert_eq!(CD_MODE1_DATA_OFFSET, 16);

        let pvd_byte_offset = PVD_SECTOR * CD_SECTOR_SIZE_COOKED as u64;
        assert_eq!(pvd_byte_offset, 32768);
    }

    #[test]
    fn test_parse_cht2_entry() {
        let content = "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:16227 PREGAP:150";
        let track = parse_cht2_entry(content, 0).unwrap();
        assert_eq!(track.track_num, 1);
        assert_eq!(track.track_type, "MODE1_RAW");
        assert_eq!(track.frames, 16227);
        assert_eq!(track.frame_offset, 0);
    }

    #[test]
    fn test_is_data_track() {
        assert!(is_data_track("MODE1_RAW"));
        assert!(is_data_track("MODE1"));
        assert!(is_data_track("MODE2_RAW"));
        assert!(!is_data_track("AUDIO"));
    }
}
