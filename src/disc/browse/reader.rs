//! Sector reader trait and implementations for different disc formats

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Logical sector size for CD-ROM data (cooked)
pub const SECTOR_SIZE: u64 = 2048;

/// Raw CD-ROM sector size
pub const RAW_SECTOR_SIZE: u64 = 2352;

/// Trait for reading logical sectors from different disc formats
pub trait SectorReader: Send {
    /// Read a single sector at the given LBA (Logical Block Address)
    fn read_sector(&mut self, lba: u64) -> Result<Vec<u8>, io::Error>;

    /// Read multiple contiguous sectors
    fn read_sectors(&mut self, start_lba: u64, count: u64) -> Result<Vec<u8>, io::Error>;

    /// Read raw bytes at a logical byte offset
    fn read_bytes(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, io::Error>;

    /// Get the logical sector size (typically 2048)
    fn sector_size(&self) -> u64 {
        SECTOR_SIZE
    }
}

/// Sector reader for standard ISO/Toast files (direct access)
pub struct IsoSectorReader {
    file: BufReader<File>,
}

impl IsoSectorReader {
    /// Create a new ISO sector reader
    pub fn new(path: &Path) -> Result<Self, io::Error> {
        let file = File::open(path)?;
        Ok(Self {
            file: BufReader::new(file),
        })
    }
}

impl SectorReader for IsoSectorReader {
    fn read_sector(&mut self, lba: u64) -> Result<Vec<u8>, io::Error> {
        let offset = lba * SECTOR_SIZE;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; SECTOR_SIZE as usize];
        self.file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn read_sectors(&mut self, start_lba: u64, count: u64) -> Result<Vec<u8>, io::Error> {
        let offset = start_lba * SECTOR_SIZE;
        let length = (count * SECTOR_SIZE) as usize;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; length];
        self.file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn read_bytes(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, io::Error> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0u8; length];
        self.file.read_exact(&mut buffer)?;
        Ok(buffer)
    }
}

/// Track information for BIN/CUE reading
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// Track number (1-based)
    pub number: u32,
    /// Physical byte offset in the BIN file
    pub file_offset: u64,
    /// Sector size (2048, 2336, or 2352)
    pub sector_size: u64,
    /// Offset within raw sector to user data (0 for cooked, 16 for Mode1 raw, 24 for Mode2)
    pub data_offset: u64,
    /// Number of sectors in this track
    pub sector_count: u64,
    /// Is this a data track?
    pub is_data: bool,
}

/// Sector reader for BIN/CUE disc images
pub struct BinCueSectorReader {
    file: BufReader<File>,
    /// First data track info
    data_track: TrackInfo,
}

impl BinCueSectorReader {
    /// Create a new BIN/CUE sector reader
    pub fn new(bin_path: &Path, track: TrackInfo) -> Result<Self, io::Error> {
        let file = File::open(bin_path)?;
        Ok(Self {
            file: BufReader::new(file),
            data_track: track,
        })
    }

    /// Calculate the physical byte offset for a given LBA
    fn calculate_offset(&self, lba: u64) -> u64 {
        self.data_track.file_offset
            + (lba * self.data_track.sector_size)
            + self.data_track.data_offset
    }
}

impl SectorReader for BinCueSectorReader {
    fn read_sector(&mut self, lba: u64) -> Result<Vec<u8>, io::Error> {
        let physical_offset = self.calculate_offset(lba);
        self.file.seek(SeekFrom::Start(physical_offset))?;
        let mut buffer = vec![0u8; SECTOR_SIZE as usize];
        self.file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn read_sectors(&mut self, start_lba: u64, count: u64) -> Result<Vec<u8>, io::Error> {
        let mut result = Vec::with_capacity((count * SECTOR_SIZE) as usize);
        for i in 0..count {
            let sector = self.read_sector(start_lba + i)?;
            result.extend_from_slice(&sector);
        }
        Ok(result)
    }

    fn read_bytes(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, io::Error> {
        // Convert byte offset to sector + offset within sector
        let start_lba = offset / SECTOR_SIZE;
        let offset_in_sector = (offset % SECTOR_SIZE) as usize;

        // Calculate how many sectors we need to read
        let end_offset = offset + length as u64;
        let end_lba = (end_offset + SECTOR_SIZE - 1) / SECTOR_SIZE;
        let sector_count = end_lba - start_lba;

        // Read all needed sectors
        let sectors = self.read_sectors(start_lba, sector_count)?;

        // Extract the requested bytes
        Ok(sectors[offset_in_sector..offset_in_sector + length].to_vec())
    }
}

/// Sector reader for CHD disc images
pub struct ChdSectorReader {
    /// CHD file handle
    chd: chd::Chd<BufReader<File>>,
    /// Hunk size in bytes
    hunk_size: u32,
    /// Frame offset of the first data track
    track_frame_offset: u64,
    /// Bytes per frame (2448 for CD-ROM with subcode)
    frame_size: u32,
    /// Offset within frame to user data
    data_offset: usize,
    /// Cached hunk data
    cached_hunk: Option<(u32, Vec<u8>)>,
    /// Hunksized buffer for decompression
    hunk_buffer: Vec<u8>,
}

impl ChdSectorReader {
    /// Create a new CHD sector reader
    pub fn new(
        chd: chd::Chd<BufReader<File>>,
        hunk_size: u32,
        track_frame_offset: u64,
        frame_size: u32,
        data_offset: usize,
    ) -> Self {
        let hunk_buffer = chd.get_hunksized_buffer();
        Self {
            chd,
            hunk_size,
            track_frame_offset,
            frame_size,
            data_offset,
            cached_hunk: None,
            hunk_buffer,
        }
    }

    /// Read a hunk, using cache if available
    fn read_hunk(&mut self, hunk_index: u32) -> Result<Vec<u8>, io::Error> {
        if let Some((cached_index, ref data)) = self.cached_hunk {
            if cached_index == hunk_index {
                return Ok(data.clone());
            }
        }

        let mut compressed_buf = Vec::new();
        self.chd
            .hunk(hunk_index)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{:?}", e)))?
            .read_hunk_in(&mut compressed_buf, &mut self.hunk_buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{:?}", e)))?;

        let result = self.hunk_buffer.clone();
        self.cached_hunk = Some((hunk_index, result.clone()));
        Ok(result)
    }
}

impl SectorReader for ChdSectorReader {
    fn read_sector(&mut self, lba: u64) -> Result<Vec<u8>, io::Error> {
        // Calculate frame byte offset
        let frame_byte_offset = self.track_frame_offset + (lba * self.frame_size as u64);
        let physical_offset = frame_byte_offset + self.data_offset as u64;

        // Determine which hunk contains this offset
        let hunk_index = (physical_offset / self.hunk_size as u64) as u32;
        let offset_in_hunk = (physical_offset % self.hunk_size as u64) as usize;

        let hunk_data = self.read_hunk(hunk_index)?;

        // Check if we need to read across hunk boundary
        if offset_in_hunk + SECTOR_SIZE as usize <= hunk_data.len() {
            Ok(hunk_data[offset_in_hunk..offset_in_hunk + SECTOR_SIZE as usize].to_vec())
        } else {
            // Read spans two hunks
            let mut result = vec![0u8; SECTOR_SIZE as usize];
            let first_part_len = hunk_data.len() - offset_in_hunk;
            result[..first_part_len].copy_from_slice(&hunk_data[offset_in_hunk..]);

            let next_hunk = self.read_hunk(hunk_index + 1)?;
            let second_part_len = SECTOR_SIZE as usize - first_part_len;
            result[first_part_len..].copy_from_slice(&next_hunk[..second_part_len]);

            Ok(result)
        }
    }

    fn read_sectors(&mut self, start_lba: u64, count: u64) -> Result<Vec<u8>, io::Error> {
        let mut result = Vec::with_capacity((count * SECTOR_SIZE) as usize);
        for i in 0..count {
            let sector = self.read_sector(start_lba + i)?;
            result.extend_from_slice(&sector);
        }
        Ok(result)
    }

    fn read_bytes(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, io::Error> {
        // Convert byte offset to sector + offset within sector
        let start_lba = offset / SECTOR_SIZE;
        let offset_in_sector = (offset % SECTOR_SIZE) as usize;

        // Calculate how many sectors we need to read
        let end_offset = offset + length as u64;
        let end_lba = (end_offset + SECTOR_SIZE - 1) / SECTOR_SIZE;
        let sector_count = end_lba - start_lba;

        // Read all needed sectors
        let sectors = self.read_sectors(start_lba, sector_count)?;

        // Extract the requested bytes
        Ok(sectors[offset_in_sector..offset_in_sector + length].to_vec())
    }
}
