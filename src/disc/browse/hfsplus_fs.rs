//! HFS+ filesystem implementation for directory listing and file reading

use super::entry::{EntryType, FileEntry};
use super::filesystem::{Filesystem, FilesystemError};
use super::reader::SectorReader;
use crate::disc::DiscInfo;

/// HFS+ record types
const HFSPLUS_FOLDER_RECORD: i16 = 1;
const HFSPLUS_FILE_RECORD: i16 = 2;
const HFSPLUS_FOLDER_THREAD_RECORD: i16 = 3;
#[allow(dead_code)]
const HFSPLUS_FILE_THREAD_RECORD: i16 = 4;

/// HFS+ root folder CNID
const HFSPLUS_ROOT_FOLDER_ID: u32 = 2;

/// HFS+ filesystem implementation
pub struct HfsPlusFilesystem {
    reader: Box<dyn SectorReader>,
    /// Offset to HFS+ partition (for APM)
    partition_offset: u64,
    /// Block size in bytes
    block_size: u32,
    /// Catalog file start block
    catalog_start_block: u32,
    /// Catalog file block count
    catalog_block_count: u32,
    /// B-tree node size
    node_size: u16,
    /// First leaf node
    first_leaf_node: u32,
    /// Volume name
    volume_name: String,
}

/// HFS+ extent descriptor
#[derive(Debug, Clone, Copy)]
struct HfsPlusExtent {
    start_block: u32,
    block_count: u32,
}

/// HFS+ fork data (simplified)
#[derive(Debug, Clone)]
struct HfsPlusForkData {
    logical_size: u64,
    extents: Vec<HfsPlusExtent>,
}

impl HfsPlusFilesystem {
    /// Create a new HFS+ filesystem from a sector reader
    pub fn new(
        mut reader: Box<dyn SectorReader>,
        partition_offset: u64,
        disc_info: &DiscInfo,
    ) -> Result<Self, FilesystemError> {
        // Read volume header at offset 1024 from partition start
        let header_offset = partition_offset + 1024;
        let header_data = reader.read_bytes(header_offset, 512)?;

        // Parse signature
        let signature = u16::from_be_bytes([header_data[0], header_data[1]]);
        if signature != 0x482B && signature != 0x4858 {
            return Err(FilesystemError::Parse(format!(
                "Invalid HFS+ signature: 0x{:04X}",
                signature
            )));
        }

        // Parse block size (bytes 40-43)
        let block_size = u32::from_be_bytes([
            header_data[40],
            header_data[41],
            header_data[42],
            header_data[43],
        ]);

        // Parse catalog file extent (first extent at bytes 128-135)
        let catalog_start_block = u32::from_be_bytes([
            header_data[128],
            header_data[129],
            header_data[130],
            header_data[131],
        ]);
        let catalog_block_count = u32::from_be_bytes([
            header_data[132],
            header_data[133],
            header_data[134],
            header_data[135],
        ]);

        // Read B-tree header from catalog file
        let catalog_offset = partition_offset + (catalog_start_block as u64 * block_size as u64);
        let btree_header = reader.read_bytes(catalog_offset, 256)?;

        // Parse node descriptor and B-tree header
        let node_kind = btree_header[8] as i8;
        if node_kind != 1 {
            return Err(FilesystemError::Parse(format!(
                "Expected B-tree header node, got kind {}",
                node_kind
            )));
        }

        // B-tree header record starts after 14-byte node descriptor
        let first_leaf_node = u32::from_be_bytes([
            btree_header[24],
            btree_header[25],
            btree_header[26],
            btree_header[27],
        ]);
        let node_size = u16::from_be_bytes([btree_header[32], btree_header[33]]);

        // Get volume name from disc info if available
        let volume_name = disc_info
            .hfsplus_header
            .as_ref()
            .map(|_| disc_info.volume_label.clone().unwrap_or_else(|| "HFS+ Volume".to_string()))
            .unwrap_or_else(|| "HFS+ Volume".to_string());

        Ok(Self {
            reader,
            partition_offset,
            block_size,
            catalog_start_block,
            catalog_block_count,
            node_size,
            first_leaf_node,
            volume_name,
        })
    }

    /// Calculate catalog file offset
    fn catalog_offset(&self) -> u64 {
        self.partition_offset + (self.catalog_start_block as u64 * self.block_size as u64)
    }

    /// Read a B-tree node
    fn read_node(&mut self, node_num: u32) -> Result<Vec<u8>, FilesystemError> {
        let offset = self.catalog_offset() + (node_num as u64 * self.node_size as u64);
        self.reader
            .read_bytes(offset, self.node_size as usize)
            .map_err(FilesystemError::from)
    }

    /// List entries in a directory by parent CNID
    fn list_directory_by_cnid(
        &mut self,
        parent_cnid: u32,
        parent_path: &str,
    ) -> Result<Vec<FileEntry>, FilesystemError> {
        let mut entries = Vec::new();
        let mut current_node = self.first_leaf_node;
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 10000;

        while current_node != 0 && attempts < MAX_ATTEMPTS {
            attempts += 1;

            let node_data = self.read_node(current_node)?;

            // Parse node descriptor
            let next_node = u32::from_be_bytes([
                node_data[0],
                node_data[1],
                node_data[2],
                node_data[3],
            ]);
            let node_kind = node_data[8] as i8;
            let num_records = u16::from_be_bytes([node_data[10], node_data[11]]);

            if node_kind != -1 {
                // Not a leaf node
                current_node = next_node;
                continue;
            }

            // Process records in this leaf node
            self.process_leaf_node(&node_data, num_records, parent_cnid, parent_path, &mut entries);

            current_node = next_node;
        }

        // Sort entries: directories first, then by name
        entries.sort_by(|a, b| {
            match (a.entry_type, b.entry_type) {
                (EntryType::Directory, EntryType::File) => std::cmp::Ordering::Less,
                (EntryType::File, EntryType::Directory) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });

        Ok(entries)
    }

    /// Process records in a leaf node
    fn process_leaf_node(
        &self,
        node_data: &[u8],
        num_records: u16,
        parent_cnid: u32,
        parent_path: &str,
        entries: &mut Vec<FileEntry>,
    ) {
        let offsets_start = self.node_size as usize - 2;

        for i in 0..num_records {
            let offset_pos = offsets_start - (i as usize * 2);
            if offset_pos + 2 > node_data.len() {
                continue;
            }

            let record_offset =
                u16::from_be_bytes([node_data[offset_pos], node_data[offset_pos + 1]]) as usize;
            if record_offset + 10 > node_data.len() {
                continue;
            }

            // Parse catalog key
            let key_length =
                u16::from_be_bytes([node_data[record_offset], node_data[record_offset + 1]])
                    as usize;
            if key_length < 6 {
                continue;
            }

            let record_parent_id = u32::from_be_bytes([
                node_data[record_offset + 2],
                node_data[record_offset + 3],
                node_data[record_offset + 4],
                node_data[record_offset + 5],
            ]);

            // Only process records with matching parent
            if record_parent_id != parent_cnid {
                continue;
            }

            // Parse name
            let name_length =
                u16::from_be_bytes([node_data[record_offset + 6], node_data[record_offset + 7]])
                    as usize;

            if name_length == 0 {
                continue; // Thread record
            }

            let name_start = record_offset + 8;
            let name_end = name_start + (name_length * 2);
            if name_end > node_data.len() {
                continue;
            }

            let name = self.decode_utf16_be(&node_data[name_start..name_end]);
            if name.is_empty() {
                continue;
            }

            // Record data starts after key
            let data_offset = record_offset + 2 + key_length;
            if data_offset + 4 > node_data.len() {
                continue;
            }

            let record_type =
                i16::from_be_bytes([node_data[data_offset], node_data[data_offset + 1]]);

            let path = if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path, name)
            };

            match record_type {
                HFSPLUS_FOLDER_RECORD => {
                    // Folder record - CNID is at data_offset + 8
                    if data_offset + 12 > node_data.len() {
                        continue;
                    }
                    let cnid = u32::from_be_bytes([
                        node_data[data_offset + 8],
                        node_data[data_offset + 9],
                        node_data[data_offset + 10],
                        node_data[data_offset + 11],
                    ]);
                    entries.push(FileEntry::new_directory(name, path, cnid as u64));
                }
                HFSPLUS_FILE_RECORD => {
                    // File record - CNID at data_offset + 8, data fork at data_offset + 88
                    if data_offset + 96 > node_data.len() {
                        continue;
                    }
                    let cnid = u32::from_be_bytes([
                        node_data[data_offset + 8],
                        node_data[data_offset + 9],
                        node_data[data_offset + 10],
                        node_data[data_offset + 11],
                    ]);
                    // Data fork logical size at data_offset + 88 (8 bytes)
                    let data_size = u64::from_be_bytes([
                        node_data[data_offset + 88],
                        node_data[data_offset + 89],
                        node_data[data_offset + 90],
                        node_data[data_offset + 91],
                        node_data[data_offset + 92],
                        node_data[data_offset + 93],
                        node_data[data_offset + 94],
                        node_data[data_offset + 95],
                    ]);
                    entries.push(FileEntry::new_file(name, path, data_size, cnid as u64));
                }
                _ => {}
            }
        }
    }

    /// Decode UTF-16 BE bytes to String
    fn decode_utf16_be(&self, bytes: &[u8]) -> String {
        let utf16_chars: Vec<u16> = bytes
            .chunks(2)
            .filter_map(|chunk| {
                if chunk.len() == 2 {
                    Some(u16::from_be_bytes([chunk[0], chunk[1]]))
                } else {
                    None
                }
            })
            .collect();

        String::from_utf16(&utf16_chars).unwrap_or_default()
    }

    /// Find file record by CNID and get its fork data
    fn find_file_fork(&mut self, cnid: u32) -> Result<HfsPlusForkData, FilesystemError> {
        let mut current_node = self.first_leaf_node;
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 10000;

        while current_node != 0 && attempts < MAX_ATTEMPTS {
            attempts += 1;

            let node_data = self.read_node(current_node)?;

            let next_node = u32::from_be_bytes([
                node_data[0],
                node_data[1],
                node_data[2],
                node_data[3],
            ]);
            let node_kind = node_data[8] as i8;
            let num_records = u16::from_be_bytes([node_data[10], node_data[11]]);

            if node_kind != -1 {
                current_node = next_node;
                continue;
            }

            // Search for file record with matching CNID
            if let Some(fork) = self.search_node_for_file(&node_data, num_records, cnid) {
                return Ok(fork);
            }

            current_node = next_node;
        }

        Err(FilesystemError::NotFound(format!("File CNID {} not found", cnid)))
    }

    /// Search a node for a file record
    fn search_node_for_file(
        &self,
        node_data: &[u8],
        num_records: u16,
        target_cnid: u32,
    ) -> Option<HfsPlusForkData> {
        let offsets_start = self.node_size as usize - 2;

        for i in 0..num_records {
            let offset_pos = offsets_start - (i as usize * 2);
            if offset_pos + 2 > node_data.len() {
                continue;
            }

            let record_offset =
                u16::from_be_bytes([node_data[offset_pos], node_data[offset_pos + 1]]) as usize;
            if record_offset + 10 > node_data.len() {
                continue;
            }

            let key_length =
                u16::from_be_bytes([node_data[record_offset], node_data[record_offset + 1]])
                    as usize;
            if key_length < 6 {
                continue;
            }

            let data_offset = record_offset + 2 + key_length;
            if data_offset + 104 > node_data.len() {
                continue;
            }

            let record_type =
                i16::from_be_bytes([node_data[data_offset], node_data[data_offset + 1]]);
            if record_type != HFSPLUS_FILE_RECORD {
                continue;
            }

            let cnid = u32::from_be_bytes([
                node_data[data_offset + 8],
                node_data[data_offset + 9],
                node_data[data_offset + 10],
                node_data[data_offset + 11],
            ]);

            if cnid != target_cnid {
                continue;
            }

            // Found it! Parse data fork
            // Data fork starts at offset 88 from record data start
            let fork_offset = data_offset + 88;
            let logical_size = u64::from_be_bytes([
                node_data[fork_offset],
                node_data[fork_offset + 1],
                node_data[fork_offset + 2],
                node_data[fork_offset + 3],
                node_data[fork_offset + 4],
                node_data[fork_offset + 5],
                node_data[fork_offset + 6],
                node_data[fork_offset + 7],
            ]);

            // First extent at fork_offset + 16
            let ext_offset = fork_offset + 16;
            let start_block = u32::from_be_bytes([
                node_data[ext_offset],
                node_data[ext_offset + 1],
                node_data[ext_offset + 2],
                node_data[ext_offset + 3],
            ]);
            let block_count = u32::from_be_bytes([
                node_data[ext_offset + 4],
                node_data[ext_offset + 5],
                node_data[ext_offset + 6],
                node_data[ext_offset + 7],
            ]);

            return Some(HfsPlusForkData {
                logical_size,
                extents: vec![HfsPlusExtent {
                    start_block,
                    block_count,
                }],
            });
        }

        None
    }
}

impl Filesystem for HfsPlusFilesystem {
    fn root(&mut self) -> Result<FileEntry, FilesystemError> {
        Ok(FileEntry::root(HFSPLUS_ROOT_FOLDER_ID as u64))
    }

    fn list_directory(&mut self, entry: &FileEntry) -> Result<Vec<FileEntry>, FilesystemError> {
        if entry.entry_type != EntryType::Directory {
            return Err(FilesystemError::NotADirectory(entry.path.clone()));
        }

        let cnid = if entry.path == "/" {
            HFSPLUS_ROOT_FOLDER_ID
        } else {
            entry.location as u32
        };

        self.list_directory_by_cnid(cnid, &entry.path)
    }

    fn read_file(&mut self, entry: &FileEntry) -> Result<Vec<u8>, FilesystemError> {
        if entry.entry_type != EntryType::File {
            return Err(FilesystemError::NotADirectory(format!(
                "{} is not a file",
                entry.path
            )));
        }

        let fork = self.find_file_fork(entry.location as u32)?;

        if fork.extents.is_empty() || fork.extents[0].block_count == 0 {
            return Ok(Vec::new());
        }

        // Read from first extent (simplified - real impl would handle multiple extents)
        let extent = &fork.extents[0];
        let offset =
            self.partition_offset + (extent.start_block as u64 * self.block_size as u64);
        let bytes_to_read = fork.logical_size as usize;

        let data = self.reader.read_bytes(offset, bytes_to_read)?;
        Ok(data)
    }

    fn read_file_range(
        &mut self,
        entry: &FileEntry,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, FilesystemError> {
        if entry.entry_type != EntryType::File {
            return Err(FilesystemError::NotADirectory(format!(
                "{} is not a file",
                entry.path
            )));
        }

        let fork = self.find_file_fork(entry.location as u32)?;

        if fork.extents.is_empty() || fork.extents[0].block_count == 0 {
            return Ok(Vec::new());
        }

        let actual_length =
            std::cmp::min(length as u64, fork.logical_size.saturating_sub(offset)) as usize;
        if actual_length == 0 {
            return Ok(Vec::new());
        }

        // Read from first extent (simplified)
        let extent = &fork.extents[0];
        let file_offset =
            self.partition_offset + (extent.start_block as u64 * self.block_size as u64) + offset;

        let data = self.reader.read_bytes(file_offset, actual_length)?;
        Ok(data)
    }

    fn volume_name(&self) -> Option<&str> {
        if self.volume_name.is_empty() {
            None
        } else {
            Some(&self.volume_name)
        }
    }
}
