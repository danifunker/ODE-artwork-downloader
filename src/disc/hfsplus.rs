//! HFS+ (Mac OS Extended) Volume Header parsing
//!
//! Reference: https://developer.apple.com/library/archive/technotes/tn/tn1150.html#VolumeHeader

use std::io::{Read, Seek, SeekFrom};

/// HFS+ Catalog Node ID for root folder
const HFSPLUS_ROOT_FOLDER_ID: u32 = 2;

/// HFS+ Folder Thread Record type
const HFSPLUS_FOLDER_THREAD_RECORD: i16 = 3;

/// HFS+ Extent Descriptor
#[derive(Debug, Clone, Copy)]
pub struct HfsPlusExtent {
    pub start_block: u32,
    pub block_count: u32,
}

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
    /// First extent of catalog file
    pub catalog_file_extent: HfsPlusExtent,
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
        // Remember current position (start of volume header)
        let header_start = reader.stream_position()
            .map_err(|e| format!("Failed to get stream position: {}", e))?;

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

        // Parse catalog file extent (first extent at bytes 128-135 within catalog fork data)
        // Catalog fork data starts at byte 112, extents start at offset 16 within fork data
        // So first extent is at bytes 128-135 of the volume header
        let catalog_start_block = u32::from_be_bytes([buffer[128], buffer[129], buffer[130], buffer[131]]);
        let catalog_block_count = u32::from_be_bytes([buffer[132], buffer[133], buffer[134], buffer[135]]);

        let catalog_file_extent = HfsPlusExtent {
            start_block: catalog_start_block,
            block_count: catalog_block_count,
        };

        log::debug!("HFS+ catalog file extent: start_block={}, block_count={}, block_size={}",
            catalog_start_block, catalog_block_count, block_size);

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
            catalog_file_extent,
        };

        // Calculate volume start position (1024 bytes before header)
        let volume_start = header_start.saturating_sub(1024);

        // Try to extract volume name from Catalog File
        let volume_name = Self::extract_volume_name_from_catalog(reader, &header, volume_start)
            .unwrap_or_else(|e| {
                log::debug!("Failed to extract HFS+ volume name: {}", e);
                String::from("HFS+ Volume")
            });

        Ok((header, volume_name))
    }

    /// Extract volume name from HFS+ Catalog File
    /// The volume name is stored in the folder thread record for the root folder (CNID 2)
    fn extract_volume_name_from_catalog<R: Read + Seek>(
        reader: &mut R,
        header: &HfsPlusVolumeHeader,
        volume_start: u64,
    ) -> Result<String, String> {
        if header.catalog_file_extent.start_block == 0 {
            return Err("No catalog file extent".to_string());
        }

        // Calculate catalog file position
        let catalog_offset = volume_start +
            (header.catalog_file_extent.start_block as u64 * header.block_size as u64);

        log::debug!("Reading HFS+ catalog at offset {}", catalog_offset);

        // Read B-tree header node (node 0)
        reader.seek(SeekFrom::Start(catalog_offset))
            .map_err(|e| format!("Failed to seek to catalog: {}", e))?;

        let mut node_header = [0u8; 14];
        reader.read_exact(&mut node_header)
            .map_err(|e| format!("Failed to read B-tree header: {}", e))?;

        // Parse B-tree node descriptor
        // Bytes 0-3: fLink (next node)
        // Bytes 4-7: bLink (previous node)
        // Byte 8: kind (node type: -1=leaf, 0=index, 1=header, 2=map)
        // Byte 9: height
        // Bytes 10-11: numRecords
        // Bytes 12-13: reserved
        let node_kind = node_header[8] as i8;
        if node_kind != 1 {
            return Err(format!("Expected header node (kind 1), got {}", node_kind));
        }

        // Read B-tree header record (after the 14-byte node descriptor)
        let mut btree_header = [0u8; 106];
        reader.read_exact(&mut btree_header)
            .map_err(|e| format!("Failed to read B-tree header record: {}", e))?;

        // Parse B-tree header
        // Bytes 0-1: treeDepth
        // Bytes 2-5: rootNode
        // Bytes 6-9: leafRecords
        // Bytes 10-13: firstLeafNode
        // Bytes 14-17: lastLeafNode
        // Bytes 18-19: nodeSize
        let root_node = u32::from_be_bytes([btree_header[2], btree_header[3], btree_header[4], btree_header[5]]);
        let first_leaf_node = u32::from_be_bytes([btree_header[10], btree_header[11], btree_header[12], btree_header[13]]);
        let node_size = u16::from_be_bytes([btree_header[18], btree_header[19]]) as u64;

        log::debug!("HFS+ B-tree: root_node={}, first_leaf={}, node_size={}",
            root_node, first_leaf_node, node_size);

        if node_size == 0 || node_size > 32768 {
            return Err(format!("Invalid node size: {}", node_size));
        }

        // Search leaf nodes for the root folder thread record
        // Start with the first leaf node
        let mut current_node = first_leaf_node;
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 1000;

        while current_node != 0 && attempts < MAX_ATTEMPTS {
            attempts += 1;

            let node_offset = catalog_offset + (current_node as u64 * node_size);
            reader.seek(SeekFrom::Start(node_offset))
                .map_err(|e| format!("Failed to seek to node {}: {}", current_node, e))?;

            let mut node_data = vec![0u8; node_size as usize];
            reader.read_exact(&mut node_data)
                .map_err(|e| format!("Failed to read node {}: {}", current_node, e))?;

            // Parse node descriptor
            let next_node = u32::from_be_bytes([node_data[0], node_data[1], node_data[2], node_data[3]]);
            let node_kind = node_data[8] as i8;
            let num_records = u16::from_be_bytes([node_data[10], node_data[11]]);

            if node_kind != -1 {
                // Not a leaf node, skip
                current_node = next_node;
                continue;
            }

            // Search records in this leaf node
            if let Some(name) = Self::search_node_for_volume_name(&node_data, num_records, node_size as u16) {
                return Ok(name);
            }

            current_node = next_node;
        }

        Err("Volume name not found in catalog".to_string())
    }

    /// Search a leaf node for the root folder thread record
    fn search_node_for_volume_name(node_data: &[u8], num_records: u16, node_size: u16) -> Option<String> {
        // Record offsets are stored at the end of the node, working backwards
        // Each offset is 2 bytes
        let offsets_start = node_size as usize - 2;

        for i in 0..num_records {
            // Get record offset from the end of the node
            let offset_pos = offsets_start - (i as usize * 2);
            if offset_pos + 2 > node_data.len() {
                continue;
            }

            let record_offset = u16::from_be_bytes([node_data[offset_pos], node_data[offset_pos + 1]]) as usize;
            if record_offset + 10 > node_data.len() {
                continue;
            }

            // Parse catalog key
            // Bytes 0-1: keyLength
            // Bytes 2-5: parentID
            // Bytes 6-7: name length (in Unicode characters)
            // Bytes 8+: name (Unicode, big-endian)
            let key_length = u16::from_be_bytes([node_data[record_offset], node_data[record_offset + 1]]) as usize;
            if key_length < 6 {
                continue;
            }

            let parent_id = u32::from_be_bytes([
                node_data[record_offset + 2],
                node_data[record_offset + 3],
                node_data[record_offset + 4],
                node_data[record_offset + 5],
            ]);

            // We're looking for the thread record with parentID = 2 (root folder)
            // and an empty name (name length = 0)
            if parent_id != HFSPLUS_ROOT_FOLDER_ID {
                continue;
            }

            let name_length = u16::from_be_bytes([node_data[record_offset + 6], node_data[record_offset + 7]]);
            if name_length != 0 {
                continue;
            }

            // Found potential thread record for root folder
            // Record data starts after key (key_length + 2 bytes for keyLength field)
            let data_offset = record_offset + 2 + key_length;
            if data_offset + 10 > node_data.len() {
                continue;
            }

            // Parse thread record
            // Bytes 0-1: recordType (should be 3 for folder thread)
            // Bytes 2-3: reserved
            // Bytes 4-7: parentID
            // Bytes 8-9: name length
            // Bytes 10+: name (Unicode, big-endian)
            let record_type = i16::from_be_bytes([node_data[data_offset], node_data[data_offset + 1]]);
            if record_type != HFSPLUS_FOLDER_THREAD_RECORD {
                continue;
            }

            let vol_name_length = u16::from_be_bytes([
                node_data[data_offset + 8],
                node_data[data_offset + 9],
            ]) as usize;

            if vol_name_length == 0 || vol_name_length > 255 {
                continue;
            }

            let name_start = data_offset + 10;
            let name_end = name_start + (vol_name_length * 2);
            if name_end > node_data.len() {
                continue;
            }

            // Convert UTF-16 BE to String
            let name_bytes = &node_data[name_start..name_end];
            let utf16_chars: Vec<u16> = name_bytes
                .chunks(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect();

            if let Ok(name) = String::from_utf16(&utf16_chars) {
                let trimmed = name.trim().to_string();
                if !trimmed.is_empty() {
                    log::info!("Extracted HFS+ volume name: {}", trimmed);
                    return Some(trimmed);
                }
            }
        }

        None
    }

    /// Check if this looks like a valid HFS+ volume header
    pub fn is_valid(&self) -> bool {
        (self.signature == 0x482B || self.signature == 0x4858) && 
        self.block_size > 0 && 
        self.total_blocks > 0
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
