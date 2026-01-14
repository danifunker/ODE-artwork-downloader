//! Apple Partition Map (APM) parsing
//!
//! Reference: https://en.wikipedia.org/wiki/Apple_Partition_Map
//! The Apple Partition Map is used on older Mac discs to divide the disc into partitions.

use std::io::{Read, Seek, SeekFrom};

/// Block size for Apple Partition Map (always 512 bytes)
const APM_BLOCK_SIZE: u64 = 512;

/// Driver Descriptor Map signature ("ER" = 0x4552)
const DDM_SIGNATURE: u16 = 0x4552;

/// Partition Map Entry signature ("PM" = 0x504D)
const PM_SIGNATURE: u16 = 0x504D;

/// Apple Partition Map Entry
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PartitionEntry {
    /// Partition signature (should be "PM" = 0x504D)
    pub signature: u16,
    /// Number of partition entries
    pub map_entries: u32,
    /// First physical block of partition
    pub start_block: u32,
    /// Number of blocks in partition
    pub block_count: u32,
    /// Partition name (32 bytes)
    pub name: String,
    /// Partition type (32 bytes, e.g., "Apple_HFS", "Apple_Driver")
    pub partition_type: String,
}

impl PartitionEntry {
    /// Parse a partition entry from a 512-byte block
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 512 {
            return Err("Partition entry data too short".to_string());
        }

        // Signature at bytes 0-1
        let signature = u16::from_be_bytes([data[0], data[1]]);
        if signature != PM_SIGNATURE {
            return Err(format!("Invalid partition signature: 0x{:04X}", signature));
        }

        // Number of partition entries at bytes 4-7
        let map_entries = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);

        // Start block at bytes 8-11
        let start_block = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        // Block count at bytes 12-15
        let block_count = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);

        // Partition name at bytes 16-47 (32 bytes, null-terminated)
        let name = String::from_utf8_lossy(&data[16..48])
            .trim_end_matches('\0')
            .trim()
            .to_string();

        // Partition type at bytes 48-79 (32 bytes, null-terminated)
        let partition_type = String::from_utf8_lossy(&data[48..80])
            .trim_end_matches('\0')
            .trim()
            .to_string();

        Ok(PartitionEntry {
            signature,
            map_entries,
            start_block,
            block_count,
            name,
            partition_type,
        })
    }

    /// Check if this partition contains HFS/HFS+ data
    pub fn is_hfs(&self) -> bool {
        self.partition_type.starts_with("Apple_HFS") || 
        self.partition_type == "Apple_HFSX"
    }
}

/// Parse Apple Partition Map and return all partitions
pub fn parse_partition_map<R: Read + Seek>(reader: &mut R) -> Result<Vec<PartitionEntry>, String> {
    // Check for Driver Descriptor Map at block 0
    reader.seek(SeekFrom::Start(0))
        .map_err(|e| format!("Failed to seek to DDM: {}", e))?;

    let mut ddm_block = [0u8; 512];
    reader.read_exact(&mut ddm_block)
        .map_err(|e| format!("Failed to read DDM: {}", e))?;

    let ddm_signature = u16::from_be_bytes([ddm_block[0], ddm_block[1]]);
    log::info!("DDM signature: 0x{:04X}", ddm_signature);
    if ddm_signature != DDM_SIGNATURE {
        return Err(format!("Invalid DDM signature: 0x{:04X} (expected 0x4552)", ddm_signature));
    }

    // Block size at bytes 2-3 (should be 512)
    let block_size = u16::from_be_bytes([ddm_block[2], ddm_block[3]]);
    log::info!("DDM block size: {}", block_size);

    // Read first partition entry at block 1
    reader.seek(SeekFrom::Start(APM_BLOCK_SIZE))
        .map_err(|e| format!("Failed to seek to first partition: {}", e))?;

    let mut first_entry_data = [0u8; 512];
    reader.read_exact(&mut first_entry_data)
        .map_err(|e| format!("Failed to read first partition entry: {}", e))?;

    let first_entry = PartitionEntry::parse(&first_entry_data)?;
    let num_partitions = first_entry.map_entries;

    log::info!("Found {} partitions in Apple Partition Map", num_partitions);

    // Read all partition entries
    let mut partitions = vec![first_entry];
    for i in 2..=num_partitions {
        reader.seek(SeekFrom::Start(i as u64 * APM_BLOCK_SIZE))
            .map_err(|e| format!("Failed to seek to partition {}: {}", i, e))?;

        let mut entry_data = [0u8; 512];
        reader.read_exact(&mut entry_data)
            .map_err(|e| format!("Failed to read partition entry {}: {}", i, e))?;

        match PartitionEntry::parse(&entry_data) {
            Ok(entry) => {
                log::info!("Partition {}: {} (type: {}, blocks: {}+{})", 
                    i, entry.name, entry.partition_type, entry.start_block, entry.block_count);
                partitions.push(entry);
            }
            Err(e) => {
                log::warn!("Failed to parse partition {}: {}", i, e);
                break;
            }
        }
    }

    Ok(partitions)
}

/// Find the first HFS/HFS+ partition and return its byte offset
pub fn find_hfs_partition_offset<R: Read + Seek>(reader: &mut R) -> Result<u64, String> {
    let partitions = parse_partition_map(reader)?;

    for partition in &partitions {
        if partition.is_hfs() {
            let offset = partition.start_block as u64 * APM_BLOCK_SIZE;
            log::info!("Found HFS partition '{}' at block {} (byte offset: {})", 
                partition.name, partition.start_block, offset);
            return Ok(offset);
        }
    }

    Err("No HFS/HFS+ partition found in partition map".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_parse_ddm() {
        let mut data = vec![0u8; 1024];
        
        // DDM signature at block 0
        data[0] = 0x45; // 'E'
        data[1] = 0x52; // 'R'
        data[2] = 0x02; // Block size = 512
        data[3] = 0x00;
        
        // First partition entry at block 1
        data[512] = 0x50; // 'P'
        data[513] = 0x4D; // 'M'
        data[516] = 0x00; // map_entries = 1
        data[517] = 0x00;
        data[518] = 0x00;
        data[519] = 0x01;
        
        let mut cursor = Cursor::new(data);
        let partitions = parse_partition_map(&mut cursor).unwrap();
        
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0].signature, 0x504D);
    }
}
