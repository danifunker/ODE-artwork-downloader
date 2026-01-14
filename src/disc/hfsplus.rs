//! HFS+ (Mac OS Extended) Volume Header parsing
//!
//! Reference: https://developer.apple.com/library/archive/technotes/tn/tn1150.html#VolumeHeader

use std::io::{Read, Seek, SeekFrom};

/// HFS+ Volume Header
/// Located at byte 1024 from the start of the volume
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HfsPlusVolumeHeader {
    /// Volume signature (0x482B for HFS+, "H+")
    pub signature: u16,
    /// Version (always 4 for HFS+, 5 for HFSX)
    pub version: u16,
    /// Volume attributes
    pub attributes: u32,
    /// Date and time of last mount
    pub last_mounted_version: u32,
    /// Journal info block
    pub journal_info_block: u32,
    /// Date and time of volume creation
    pub create_date: u32,
    /// Date and time of last modification
    pub modify_date: u32,
    /// Date and time of last backup
    pub backup_date: u32,
    /// Date and time of last check
    pub checked_date: u32,
    /// Number of files on volume
    pub file_count: u32,
    /// Number of folders on volume
    pub folder_count: u32,
    /// Block size in bytes
    pub block_size: u32,
    /// Total number of blocks
    pub total_blocks: u32,
    /// Number of free blocks
    pub free_blocks: u32,
}

/// HFS+ Finder Info
/// Contains volume name and other metadata
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HfsPlusFinderInfo {
    /// Volume name (up to 255 UTF-16 characters)
    pub volume_name: String,
}

impl HfsPlusVolumeHeader {
    /// Parse HFS+ volume header from a reader
    /// Reader should be positioned at the start of the volume
    pub fn parse<R: Read + Seek>(reader: &mut R) -> Result<(Self, String), String> {
        // Seek to byte 1024 where the volume header starts
        reader.seek(SeekFrom::Start(1024))
            .map_err(|e| format!("Failed to seek to HFS+ header: {}", e))?;

        Self::parse_from_current_position(reader)
    }

    /// Parse HFS+ volume header from the current reader position
    /// Reader should already be positioned at byte 1024 of the volume
    pub fn parse_from_current_position<R: Read + Seek>(reader: &mut R) -> Result<(Self, String), String> {
        let mut buffer = [0u8; 512]; // Volume header is 512 bytes
        reader.read_exact(&mut buffer)
            .map_err(|e| format!("Failed to read HFS+ header: {}", e))?;

        // Parse signature (bytes 0-1)
        let signature = u16::from_be_bytes([buffer[0], buffer[1]]);
        
        // Check for HFS+ signature (0x482B = "H+") or HFSX (0x4858 = "HX")
        if signature != 0x482B && signature != 0x4858 {
            return Err(format!("Invalid HFS+ signature: 0x{:04X}", signature));
        }

        // Parse version (bytes 2-3)
        let version = u16::from_be_bytes([buffer[2], buffer[3]]);

        // Parse attributes (bytes 4-7)
        let attributes = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);

        // Parse last mounted version (bytes 8-11)
        let last_mounted_version = u32::from_be_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]);

        // Parse journal info block (bytes 12-15)
        let journal_info_block = u32::from_be_bytes([buffer[12], buffer[13], buffer[14], buffer[15]]);

        // Parse creation date (bytes 16-19) - HFS+ time (seconds since Jan 1, 1904)
        let create_date = u32::from_be_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]);

        // Parse modification date (bytes 20-23)
        let modify_date = u32::from_be_bytes([buffer[20], buffer[21], buffer[22], buffer[23]]);

        // Parse backup date (bytes 24-27)
        let backup_date = u32::from_be_bytes([buffer[24], buffer[25], buffer[26], buffer[27]]);

        // Parse checked date (bytes 28-31)
        let checked_date = u32::from_be_bytes([buffer[28], buffer[29], buffer[30], buffer[31]]);

        // Parse file count (bytes 32-35)
        let file_count = u32::from_be_bytes([buffer[32], buffer[33], buffer[34], buffer[35]]);

        // Parse folder count (bytes 36-39)
        let folder_count = u32::from_be_bytes([buffer[36], buffer[37], buffer[38], buffer[39]]);

        // Parse block size (bytes 40-43)
        let block_size = u32::from_be_bytes([buffer[40], buffer[41], buffer[42], buffer[43]]);

        // Parse total blocks (bytes 44-47)
        let total_blocks = u32::from_be_bytes([buffer[44], buffer[45], buffer[46], buffer[47]]);

        // Parse free blocks (bytes 48-51)
        let free_blocks = u32::from_be_bytes([buffer[48], buffer[49], buffer[50], buffer[51]]);

        let header = HfsPlusVolumeHeader {
            signature,
            version,
            attributes,
            last_mounted_version,
            journal_info_block,
            create_date,
            modify_date,
            backup_date,
            checked_date,
            file_count,
            folder_count,
            block_size,
            total_blocks,
            free_blocks,
        };

        // Try to extract volume name from Catalog File
        // For now, we'll try to read it from the root directory
        // The volume name in HFS+ is stored in the Catalog File's root folder record
        // This is complex, so we'll use a simplified approach
        let volume_name = Self::extract_volume_name(reader)?;

        Ok((header, volume_name))
    }

    /// Extract volume name from HFS+ volume
    /// This is a simplified implementation that reads the root directory name
    fn extract_volume_name<R: Read + Seek>(_reader: &mut R) -> Result<String, String> {
        // TODO: Implement proper catalog file parsing
        // For now, return a placeholder
        // The catalog file structure is complex and requires:
        // 1. Reading the allocation file
        // 2. Parsing the B-tree structure
        // 3. Finding the root folder record
        // 4. Extracting the Unicode name
        
        Ok(String::from("HFS+ Volume"))
    }

    /// Check if this looks like a valid HFS+ volume header
    pub fn is_valid(&self) -> bool {
        (self.signature == 0x482B || self.signature == 0x4858) && 
        self.block_size > 0 && 
        self.total_blocks > 0
    }

    /// Read HFS+ Volume Header from a specific offset
    pub fn read_at_offset<R: Read + Seek>(reader: &mut R, offset: u64) -> Result<(Self, String), String> {
        log::debug!("Attempting to read HFS+ header at offset {}", offset);
        reader.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("Failed to seek to HFS+ header at offset {}: {}", offset, e))?;
        Self::parse_from_current_position(reader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_parse_hfsplus_header() {
        // Create a minimal HFS+ volume header structure
        let mut data = vec![0u8; 2048];
        
        // Signature at byte 1024
        data[1024] = 0x48; // 'H'
        data[1025] = 0x2B; // '+'
        
        // Version (4)
        data[1026] = 0x00;
        data[1027] = 0x04;
        
        // Block size (4096)
        data[1064] = 0x00;
        data[1065] = 0x00;
        data[1066] = 0x10;
        data[1067] = 0x00;
        
        // Total blocks (1000)
        data[1068] = 0x00;
        data[1069] = 0x00;
        data[1070] = 0x03;
        data[1071] = 0xE8;
        
        let mut cursor = Cursor::new(data);
        let (header, _name) = HfsPlusVolumeHeader::parse(&mut cursor).unwrap();
        
        assert_eq!(header.signature, 0x482B);
        assert_eq!(header.version, 4);
        assert_eq!(header.block_size, 4096);
        assert_eq!(header.total_blocks, 1000);
        assert!(header.is_valid());
    }
}
