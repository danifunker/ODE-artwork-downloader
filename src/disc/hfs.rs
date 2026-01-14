//! HFS (Hierarchical File System) Master Directory Block parsing
//!
//! Reference: https://developer.apple.com/library/archive/documentation/mac/Files/Files-102.html

use std::io::{Read, Seek, SeekFrom};

/// HFS Master Directory Block (MDB)
/// Located at byte 1024 from the start of the volume
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MasterDirectoryBlock {
    /// Volume signature (0x4244 for HFS, "BD")
    pub signature: u16,
    /// Date and time of volume creation
    pub create_date: u32,
    /// Date and time of last modification
    pub modify_date: u32,
    /// Volume attributes
    pub attributes: u16,
    /// Number of files in root directory
    pub root_file_count: u16,
    /// Number of directories in root directory
    pub root_dir_count: u16,
    /// Number of allocation blocks
    pub alloc_blocks: u16,
    /// Size of allocation blocks in bytes
    pub alloc_block_size: u32,
    /// Volume name (1-27 characters, Pascal string)
    pub volume_name: String,
}

impl MasterDirectoryBlock {
    /// Parse HFS MDB from a reader
    /// Reader should be positioned at the start of the volume
    pub fn parse<R: Read + Seek>(reader: &mut R) -> Result<Self, String> {
        // Seek to byte 1024 where the MDB starts
        reader.seek(SeekFrom::Start(1024))
            .map_err(|e| format!("Failed to seek to MDB: {}", e))?;

        Self::parse_from_current_position(reader)
    }

    /// Parse HFS MDB from the current reader position
    /// Reader should already be positioned at byte 1024 of the volume
    pub fn parse_from_current_position<R: Read>(reader: &mut R) -> Result<Self, String> {
        let mut buffer = [0u8; 162]; // MDB is 162 bytes
        reader.read_exact(&mut buffer)
            .map_err(|e| format!("Failed to read MDB: {}", e))?;

        // Parse signature (bytes 0-1)
        let signature = u16::from_be_bytes([buffer[0], buffer[1]]);
        
        // Check for HFS signature (0x4244 = "BD")
        if signature != 0x4244 {
            return Err(format!("Invalid HFS signature: 0x{:04X}", signature));
        }

        // Parse creation date (bytes 2-5)
        let create_date = u32::from_be_bytes([buffer[2], buffer[3], buffer[4], buffer[5]]);

        // Parse modification date (bytes 6-9)
        let modify_date = u32::from_be_bytes([buffer[6], buffer[7], buffer[8], buffer[9]]);

        // Parse attributes (bytes 10-11)
        let attributes = u16::from_be_bytes([buffer[10], buffer[11]]);

        // Parse root file count (bytes 12-13)
        let root_file_count = u16::from_be_bytes([buffer[12], buffer[13]]);

        // Parse root directory count (bytes 16-17)
        let root_dir_count = u16::from_be_bytes([buffer[16], buffer[17]]);

        // Parse number of allocation blocks (bytes 18-19)
        let alloc_blocks = u16::from_be_bytes([buffer[18], buffer[19]]);

        // Parse allocation block size (bytes 20-23)
        let alloc_block_size = u32::from_be_bytes([buffer[20], buffer[21], buffer[22], buffer[23]]);

        // Parse volume name (bytes 36-63)
        // First byte is length (Pascal string)
        let name_length = buffer[36] as usize;
        if name_length > 27 {
            return Err(format!("Invalid volume name length: {}", name_length));
        }
        
        let volume_name = String::from_utf8_lossy(&buffer[37..37 + name_length])
            .trim()
            .to_string();

        Ok(MasterDirectoryBlock {
            signature,
            create_date,
            modify_date,
            attributes,
            root_file_count,
            root_dir_count,
            alloc_blocks,
            alloc_block_size,
            volume_name,
        })
    }

    /// Check if this looks like a valid HFS MDB
    pub fn is_valid(&self) -> bool {
        self.signature == 0x4244 && !self.volume_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_parse_mdb() {
        // Create a minimal HFS MDB structure
        let mut data = vec![0u8; 2048];
        
        // Signature at byte 1024
        data[1024] = 0x42; // 'B'
        data[1025] = 0x44; // 'D'
        
        // Volume name (Pascal string) at byte 1060
        data[1060] = 7; // Length
        data[1061..1068].copy_from_slice(b"TestVol");
        
        let mut cursor = Cursor::new(data);
        let mdb = MasterDirectoryBlock::parse(&mut cursor).unwrap();
        
        assert_eq!(mdb.signature, 0x4244);
        assert_eq!(mdb.volume_name, "TestVol");
        assert!(mdb.is_valid());
    }
}
