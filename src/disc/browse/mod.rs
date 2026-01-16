//! Disc filesystem browsing module
//!
//! Provides functionality for browsing files on disc images with different
//! formats (ISO, BIN/CUE, CHD) and filesystems (ISO 9660, HFS, HFS+).

pub mod entry;
pub mod filesystem;
pub mod reader;
pub mod iso9660_fs;
pub mod hfs_fs;
pub mod hfsplus_fs;

pub use entry::{FileEntry, EntryType};
pub use filesystem::{Filesystem, FilesystemError};
pub use reader::{SectorReader, IsoSectorReader, BinCueSectorReader, ChdSectorReader, TrackInfo, SECTOR_SIZE};

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use cue_sheet::parser::{parse_cue, Command, TrackType};

use crate::disc::{DiscInfo, DiscFormat, FilesystemType};

/// CD sector size for raw data (2352 bytes)
const CD_SECTOR_SIZE_RAW: u64 = 2352;

/// Offset to user data in a raw Mode 1 sector
const MODE1_DATA_OFFSET: u64 = 16;

/// Open a filesystem from disc info, selecting the appropriate reader and filesystem implementation
pub fn open_filesystem(disc_info: &DiscInfo) -> Result<Box<dyn Filesystem>, FilesystemError> {
    match disc_info.format {
        DiscFormat::Iso => open_iso_filesystem(disc_info),
        DiscFormat::BinCue => open_bincue_filesystem(disc_info),
        DiscFormat::Chd => open_chd_filesystem(disc_info),
        _ => Err(FilesystemError::Unsupported),
    }
}

/// Open filesystem from an ISO/Toast file
fn open_iso_filesystem(disc_info: &DiscInfo) -> Result<Box<dyn Filesystem>, FilesystemError> {
    let reader = IsoSectorReader::new(&disc_info.path)?;
    create_filesystem(Box::new(reader), disc_info)
}

/// Open filesystem from a BIN/CUE file
fn open_bincue_filesystem(disc_info: &DiscInfo) -> Result<Box<dyn Filesystem>, FilesystemError> {
    // Parse the CUE file to get track info
    let (bin_path, track_info) = parse_cue_for_data_track(&disc_info.path)?;
    let reader = BinCueSectorReader::new(&bin_path, track_info)?;
    create_filesystem(Box::new(reader), disc_info)
}

/// Open filesystem from a CHD file
fn open_chd_filesystem(disc_info: &DiscInfo) -> Result<Box<dyn Filesystem>, FilesystemError> {
    let file = File::open(&disc_info.path)?;
    let buf_reader = BufReader::new(file);

    let mut chd = chd::Chd::open(buf_reader, None)
        .map_err(|e| FilesystemError::Parse(format!("Failed to open CHD: {}", e)))?;

    let hunk_size = chd.header().hunk_size();

    // Parse CHD metadata to find first data track
    let (track_frame_offset, frame_size, data_offset) = parse_chd_for_data_track(&mut chd)?;

    let reader = ChdSectorReader::new(chd, hunk_size, track_frame_offset, frame_size, data_offset);
    create_filesystem(Box::new(reader), disc_info)
}

/// Create the appropriate filesystem implementation based on detected type
fn create_filesystem(
    mut reader: Box<dyn SectorReader>,
    disc_info: &DiscInfo,
) -> Result<Box<dyn Filesystem>, FilesystemError> {
    match disc_info.filesystem {
        FilesystemType::Iso9660 => {
            Ok(Box::new(iso9660_fs::Iso9660Filesystem::new(reader)?))
        }
        FilesystemType::Hfs => {
            // Detect APM and find HFS partition offset
            let partition_offset = find_hfs_partition_offset_from_reader(reader.as_mut())
                .unwrap_or_else(|e| {
                    log::warn!("APM detection failed: {}, trying offset 0", e);
                    0
                });
            Ok(Box::new(hfs_fs::HfsFilesystem::new(reader, partition_offset, disc_info)?))
        }
        FilesystemType::HfsPlus => {
            // Detect APM and find HFS+ partition offset
            let partition_offset = find_hfs_partition_offset_from_reader(reader.as_mut())
                .unwrap_or_else(|e| {
                    log::warn!("APM detection failed: {}, trying offset 0", e);
                    0
                });
            Ok(Box::new(hfsplus_fs::HfsPlusFilesystem::new(reader, partition_offset, disc_info)?))
        }
        _ => Err(FilesystemError::Unsupported),
    }
}

/// APM block size (always 512 bytes)
const APM_BLOCK_SIZE: u64 = 512;

/// Driver Descriptor Map signature ("ER" = 0x4552)
const DDM_SIGNATURE: u16 = 0x4552;

/// Partition Map Entry signature ("PM" = 0x504D)
const PM_SIGNATURE: u16 = 0x504D;

/// Find HFS/HFS+ partition offset using a sector reader
/// This detects Apple Partition Map and finds the first HFS partition
fn find_hfs_partition_offset_from_reader(reader: &mut dyn SectorReader) -> Result<u64, FilesystemError> {
    // Read Driver Descriptor Map at block 0
    let ddm_block = reader.read_bytes(0, 512)?;

    let ddm_signature = u16::from_be_bytes([ddm_block[0], ddm_block[1]]);

    if ddm_signature != DDM_SIGNATURE {
        // No APM, check for direct HFS/HFS+ at byte 1024
        let header_block = reader.read_bytes(1024, 4)?;
        let sig = u16::from_be_bytes([header_block[0], header_block[1]]);

        // HFS signature = 0x4244 ("BD"), HFS+ = 0x482B ("H+") or 0x4858 ("HX")
        if sig == 0x4244 || sig == 0x482B || sig == 0x4858 {
            log::info!("No APM found, direct HFS/HFS+ at offset 0");
            return Ok(0);
        }

        return Err(FilesystemError::Parse(format!(
            "No Apple Partition Map (DDM signature: 0x{:04X}) and no direct HFS",
            ddm_signature
        )));
    }

    log::info!("Found Apple Partition Map (DDM signature)");

    // Read first partition entry at block 1
    let first_entry = reader.read_bytes(APM_BLOCK_SIZE, 512)?;

    let pm_sig = u16::from_be_bytes([first_entry[0], first_entry[1]]);
    if pm_sig != PM_SIGNATURE {
        return Err(FilesystemError::Parse(format!(
            "Invalid partition map signature: 0x{:04X}",
            pm_sig
        )));
    }

    // Number of partition entries at bytes 4-7
    let num_entries = u32::from_be_bytes([first_entry[4], first_entry[5], first_entry[6], first_entry[7]]);
    log::info!("APM has {} partition entries", num_entries);

    // Scan all partition entries for HFS/HFS+ partition
    for i in 1..=num_entries {
        let entry_offset = i as u64 * APM_BLOCK_SIZE;
        let entry_data = reader.read_bytes(entry_offset, 512)?;

        let sig = u16::from_be_bytes([entry_data[0], entry_data[1]]);
        if sig != PM_SIGNATURE {
            continue;
        }

        // Start block at bytes 8-11
        let start_block = u32::from_be_bytes([entry_data[8], entry_data[9], entry_data[10], entry_data[11]]);

        // Partition type at bytes 48-79 (32 bytes, null-terminated)
        let partition_type = String::from_utf8_lossy(&entry_data[48..80])
            .trim_end_matches('\0')
            .trim()
            .to_string();

        // Partition name at bytes 16-47 (32 bytes, null-terminated)
        let partition_name = String::from_utf8_lossy(&entry_data[16..48])
            .trim_end_matches('\0')
            .trim()
            .to_string();

        log::info!("Partition {}: '{}' type='{}' start_block={}", i, partition_name, partition_type, start_block);

        // Check if this is an HFS/HFS+ partition
        if partition_type.starts_with("Apple_HFS") || partition_type == "Apple_HFSX" {
            let offset = start_block as u64 * APM_BLOCK_SIZE;
            log::info!("Found HFS partition '{}' at byte offset {}", partition_name, offset);
            return Ok(offset);
        }
    }

    Err(FilesystemError::Parse("No HFS/HFS+ partition found in APM".to_string()))
}

/// Normalize CUE file keywords for parser compatibility
fn normalize_cue_keywords(content: &str) -> String {
    let mut result = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip CATALOG lines (parser has issues with number parsing)
        if trimmed.starts_with("CATALOG") {
            continue;
        }

        // Handle FILE lines - strip quotes from filename (parser quirk)
        if trimmed.starts_with("FILE ") {
            // Parse: FILE "filename" FORMAT or FILE filename FORMAT
            if let Some(rest) = trimmed.strip_prefix("FILE ") {
                let rest = rest.trim();
                if rest.starts_with('"') {
                    // Quoted filename - find closing quote
                    if let Some(end_quote) = rest[1..].find('"') {
                        let filename = &rest[1..1 + end_quote];
                        let format = rest[1 + end_quote + 1..].trim();
                        result.push_str(&format!("FILE {} {}\n", filename, format));
                        continue;
                    }
                }
            }
        }

        result.push_str(line);
        result.push('\n');
    }

    // Fix case-sensitive file format keywords
    result
        .replace("BINARY", "Binary")
        .replace("MOTOROLA", "Motorola")
        .replace(" WAVE", " Wave")
        .replace(" MP3", " Mp3")
        .replace(" AIFF", " Aiff")
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

/// Parsed track information from CUE
#[derive(Debug, Clone)]
struct ParsedTrack {
    track_no: u32,
    track_type: TrackType,
    sector_size: u64,
    data_offset: u64,
    is_data: bool,
    bin_filename: String,
}

/// Parse a CUE file to find the first data track
fn parse_cue_for_data_track(cue_path: &Path) -> Result<(std::path::PathBuf, TrackInfo), FilesystemError> {
    // Read file as bytes to handle non-UTF-8 encodings (Latin-1, etc.)
    let mut cue_bytes = Vec::new();
    File::open(cue_path)?.read_to_end(&mut cue_bytes)?;

    // Try UTF-8 first, fall back to lossy conversion (handles Latin-1, etc.)
    let cue_content = match String::from_utf8(cue_bytes.clone()) {
        Ok(s) => s,
        Err(_) => {
            log::info!("CUE file is not valid UTF-8, using lossy conversion");
            String::from_utf8_lossy(&cue_bytes).to_string()
        }
    };

    let cue_dir = cue_path.parent().unwrap_or(Path::new("."));

    // Normalize and parse CUE content
    let normalized = normalize_cue_keywords(&cue_content);
    let commands = parse_cue(&normalized)
        .map_err(|e| FilesystemError::Parse(format!("Failed to parse CUE: {:?}", e)))?;

    // Extract tracks from commands
    let mut tracks: Vec<ParsedTrack> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_track: Option<(u32, TrackType)> = None;

    for cmd in &commands {
        match cmd {
            Command::File(filename, _format) => {
                current_file = Some(filename.clone());
            }
            Command::Track(track_no, track_type) => {
                current_track = Some((*track_no as u32, track_type.clone()));
            }
            Command::Index(idx, _time) => {
                // When we see INDEX 01, save the track
                if *idx == 1 {
                    if let (Some(ref filename), Some((track_no, ref track_type))) = (&current_file, &current_track) {
                        let (sector_size, data_offset, is_data) = get_track_params(track_type);
                        tracks.push(ParsedTrack {
                            track_no: *track_no,
                            track_type: track_type.clone(),
                            sector_size,
                            data_offset,
                            is_data,
                            bin_filename: filename.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // Find first data track
    let data_track = tracks.iter().find(|t| t.is_data)
        .ok_or_else(|| FilesystemError::Parse("No data track found in CUE".to_string()))?;

    // Resolve BIN path
    let bin_path = resolve_bin_path(cue_dir, &data_track.bin_filename, Some(cue_path))?;

    // Calculate sector count from file size
    let file_size = std::fs::metadata(&bin_path)?.len();
    let sector_count = file_size / data_track.sector_size;

    Ok((
        bin_path,
        TrackInfo {
            number: data_track.track_no,
            file_offset: 0, // First data track starts at 0
            sector_size: data_track.sector_size,
            data_offset: data_track.data_offset,
            sector_count,
            is_data: true,
        },
    ))
}

/// Resolve BIN file path, trying different locations
fn resolve_bin_path(
    cue_dir: &Path,
    bin_filename: &str,
    cue_path: Option<&Path>,
) -> Result<std::path::PathBuf, FilesystemError> {
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
    if let Some(cue) = cue_path {
        if let Some(cue_stem) = cue.file_stem() {
            for ext in &["bin", "BIN", "img", "IMG"] {
                let try_path = cue_dir.join(format!("{}.{}", cue_stem.to_string_lossy(), ext));
                if try_path.exists() {
                    return Ok(try_path);
                }
            }
        }
    }

    Err(FilesystemError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("BIN file not found: {}", bin_filename),
    )))
}

use chd::metadata::MetadataTag;

/// CHD metadata tag for CD track info (CHT2 = 0x43485432)
const CHT2_TAG: u32 = 0x43485432;

/// Parse CHD metadata to find first data track info
fn parse_chd_for_data_track(
    chd: &mut chd::Chd<BufReader<File>>,
) -> Result<(u64, u32, usize), FilesystemError> {
    // CD-ROM frame size with subcode
    const CD_FRAME_SIZE: u32 = 2448;

    // Default values for Mode 1 data
    let mut track_frame_offset = 0u64;
    let frame_size = CD_FRAME_SIZE;
    let mut data_offset = 16usize; // Mode 1 raw offset

    // Collect metadata refs
    let meta_refs: Vec<_> = chd
        .metadata_refs()
        .filter(|meta_ref| meta_ref.metatag() == CHT2_TAG)
        .collect();

    // Process each track
    for meta_ref in meta_refs {
        if let Ok(metadata) = meta_ref.read(chd.inner()) {
            let meta_str = String::from_utf8_lossy(&metadata.value);

            // Parse TRACK TYPE SUBTYPE FRAMES
            // Example: "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:298441"
            for line in meta_str.lines() {
                if line.contains("TYPE:MODE1_RAW") || line.contains("TYPE:MODE1") {
                    // This is a data track
                    if line.contains("TYPE:MODE1_RAW") {
                        data_offset = 16;
                    } else {
                        data_offset = 0;
                    }
                    // Found the first data track, return
                    return Ok((track_frame_offset, frame_size, data_offset));
                } else if line.contains("TYPE:AUDIO") {
                    // Audio track - add frames to offset
                    if let Some(frames_part) = line.split_whitespace().find(|s| s.starts_with("FRAMES:")) {
                        if let Ok(frames) = frames_part[7..].parse::<u64>() {
                            track_frame_offset += frames * frame_size as u64;
                        }
                    }
                }
            }
        }
    }

    // Default: assume first track is data at offset 0
    Ok((0, frame_size, 16))
}
