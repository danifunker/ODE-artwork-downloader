//! ISO 9660 Primary Volume Descriptor parsing
//!
//! Implements reading of the Primary Volume Descriptor (PVD) from ISO 9660 disc images.
//! The PVD is located at sector 16 (offset 32768 bytes) and contains volume identification.

use std::io::{Read, Seek, SeekFrom};

/// ISO 9660 sector size in bytes
pub const SECTOR_SIZE: u64 = 2048;

/// Sector number where the Primary Volume Descriptor is located
pub const PVD_SECTOR: u64 = 16;

/// Byte offset to the PVD from start of disc
pub const PVD_OFFSET: u64 = PVD_SECTOR * SECTOR_SIZE;

/// Volume descriptor type for Primary Volume Descriptor
const PVD_TYPE: u8 = 1;

/// Standard identifier for ISO 9660 volume descriptors
const ISO9660_IDENTIFIER: &[u8; 5] = b"CD001";

/// Primary Volume Descriptor structure
///
/// Contains identifying information extracted from an ISO 9660 disc image.
#[derive(Debug, Clone)]
pub struct PrimaryVolumeDescriptor {
    /// Volume identifier (32 bytes, space-padded)
    pub volume_id: String,
    /// System identifier (32 bytes)
    pub system_id: String,
    /// Volume set identifier (128 bytes)
    pub volume_set_id: String,
    /// Publisher identifier (128 bytes)
    pub publisher_id: String,
    /// Application identifier (128 bytes)
    pub application_id: String,
}

impl PrimaryVolumeDescriptor {
    /// Read and parse the Primary Volume Descriptor from a seekable reader
    ///
    /// # Arguments
    /// * `reader` - A reader positioned at any point; will seek to PVD location
    ///
    /// # Returns
    /// * `Ok(PrimaryVolumeDescriptor)` - Successfully parsed PVD
    /// * `Err(String)` - Error message if parsing failed
    pub fn read_from<R: Read + Seek>(reader: &mut R) -> Result<Self, String> {
        // Seek to PVD location
        reader
            .seek(SeekFrom::Start(PVD_OFFSET))
            .map_err(|e| format!("Failed to seek to PVD: {}", e))?;

        // Read the full sector
        let mut sector = [0u8; SECTOR_SIZE as usize];
        reader
            .read_exact(&mut sector)
            .map_err(|e| format!("Failed to read PVD sector: {}", e))?;

        Self::parse(&sector)
    }

    /// Parse a Primary Volume Descriptor from raw sector data
    ///
    /// # Arguments
    /// * `sector` - Raw 2048-byte sector data
    ///
    /// # Returns
    /// * `Ok(PrimaryVolumeDescriptor)` - Successfully parsed PVD
    /// * `Err(String)` - Error message if parsing failed
    pub fn parse(sector: &[u8]) -> Result<Self, String> {
        if sector.len() < SECTOR_SIZE as usize {
            return Err(format!(
                "Sector too small: {} bytes (expected {})",
                sector.len(),
                SECTOR_SIZE
            ));
        }

        // Check volume descriptor type (byte 0)
        if sector[0] != PVD_TYPE {
            return Err(format!(
                "Not a Primary Volume Descriptor (type {} != {})",
                sector[0], PVD_TYPE
            ));
        }

        // Check standard identifier (bytes 1-5)
        if &sector[1..6] != ISO9660_IDENTIFIER {
            return Err("Invalid ISO 9660 identifier (expected 'CD001')".to_string());
        }

        // Check version (byte 6, should be 1)
        if sector[6] != 1 {
            return Err(format!("Unsupported PVD version: {}", sector[6]));
        }

        // Extract fields from the sector
        // Layout according to ECMA-119:
        // Offset 8-39: System Identifier (32 bytes, a-characters)
        // Offset 40-71: Volume Identifier (32 bytes, d-characters)
        // Offset 190-317: Volume Set Identifier (128 bytes)
        // Offset 318-445: Publisher Identifier (128 bytes)
        // Offset 574-701: Application Identifier (128 bytes)

        let system_id = Self::extract_string(&sector[8..40]);
        let volume_id = Self::extract_string(&sector[40..72]);
        let volume_set_id = Self::extract_string(&sector[190..318]);
        let publisher_id = Self::extract_string(&sector[318..446]);
        let application_id = Self::extract_string(&sector[574..702]);

        Ok(Self {
            volume_id,
            system_id,
            volume_set_id,
            publisher_id,
            application_id,
        })
    }

    /// Extract a string from a byte slice, trimming trailing spaces and nulls
    fn extract_string(bytes: &[u8]) -> String {
        // Convert bytes to string, handling invalid UTF-8 gracefully
        let s = String::from_utf8_lossy(bytes);
        // Trim trailing spaces and null characters
        s.trim_end_matches(|c: char| c == ' ' || c == '\0')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn create_test_pvd() -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_SIZE as usize];

        // Volume descriptor type
        sector[0] = PVD_TYPE;

        // Standard identifier "CD001"
        sector[1..6].copy_from_slice(ISO9660_IDENTIFIER);

        // Version
        sector[6] = 1;

        // Volume ID at offset 40 (32 bytes)
        let volume_id = b"TEST_VOLUME                     ";
        sector[40..72].copy_from_slice(volume_id);

        // System ID at offset 8 (32 bytes)
        let system_id = b"TEST_SYSTEM                     ";
        sector[8..40].copy_from_slice(system_id);

        sector
    }

    #[test]
    fn test_parse_pvd() {
        let sector = create_test_pvd();
        let pvd = PrimaryVolumeDescriptor::parse(&sector).unwrap();

        assert_eq!(pvd.volume_id, "TEST_VOLUME");
        assert_eq!(pvd.system_id, "TEST_SYSTEM");
    }

    #[test]
    fn test_read_from_cursor() {
        let mut data = vec![0u8; (PVD_OFFSET + SECTOR_SIZE) as usize];
        let sector = create_test_pvd();
        data[PVD_OFFSET as usize..(PVD_OFFSET + SECTOR_SIZE) as usize].copy_from_slice(&sector);

        let mut cursor = Cursor::new(data);
        let pvd = PrimaryVolumeDescriptor::read_from(&mut cursor).unwrap();

        assert_eq!(pvd.volume_id, "TEST_VOLUME");
    }

    #[test]
    fn test_invalid_identifier() {
        let mut sector = create_test_pvd();
        sector[1..6].copy_from_slice(b"XXXXX");

        let result = PrimaryVolumeDescriptor::parse(&sector);
        assert!(result.is_err());
    }
}
