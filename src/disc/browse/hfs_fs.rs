//! HFS (classic) filesystem implementation for directory listing and file reading
//!
//! Note: HFS (classic) is less common than HFS+. This is a simplified implementation
//! that handles basic directory listing but may not support all HFS features.

use super::entry::{EntryType, FileEntry};
use super::filesystem::{Filesystem, FilesystemError};
use super::reader::SectorReader;
use crate::disc::DiscInfo;

/// HFS record types
const HFS_FOLDER_RECORD: i8 = 1;
const HFS_FILE_RECORD: i8 = 2;

/// HFS root directory ID
const HFS_ROOT_DIR_ID: u32 = 2;

/// HFS filesystem implementation
pub struct HfsFilesystem {
    reader: Box<dyn SectorReader>,
    /// Offset to HFS partition
    partition_offset: u64,
    /// Allocation block size
    alloc_block_size: u32,
    /// Catalog file first allocation block
    catalog_first_block: u16,
    /// Catalog file extent length
    catalog_extent_length: u16,
    /// B-tree node size
    node_size: u16,
    /// First leaf node
    first_leaf_node: u32,
    /// First allocation block offset
    alloc_block_start: u32,
    /// Volume name
    volume_name: String,
}

impl HfsFilesystem {
    /// Create a new HFS filesystem from a sector reader
    pub fn new(
        mut reader: Box<dyn SectorReader>,
        partition_offset: u64,
        disc_info: &DiscInfo,
    ) -> Result<Self, FilesystemError> {
        // Read Master Directory Block at offset 1024
        let mdb_offset = partition_offset + 1024;
        let mdb_data = reader.read_bytes(mdb_offset, 162)?;

        // Parse signature
        let signature = u16::from_be_bytes([mdb_data[0], mdb_data[1]]);
        if signature != 0x4244 {
            return Err(FilesystemError::Parse(format!(
                "Invalid HFS signature: 0x{:04X}",
                signature
            )));
        }

        // Parse allocation block size (bytes 20-23)
        let alloc_block_size = u32::from_be_bytes([
            mdb_data[20],
            mdb_data[21],
            mdb_data[22],
            mdb_data[23],
        ]);

        // Parse first allocation block (bytes 28-29)
        let alloc_block_start = u16::from_be_bytes([mdb_data[28], mdb_data[29]]) as u32;

        // Parse catalog file extent (bytes 150-153)
        let catalog_first_block = u16::from_be_bytes([mdb_data[150], mdb_data[151]]);
        let catalog_extent_length = u16::from_be_bytes([mdb_data[152], mdb_data[153]]);

        // Parse volume name (bytes 36-63, Pascal string)
        let name_length = mdb_data[36] as usize;
        let volume_name = if name_length > 0 && name_length <= 27 {
            // Convert MacRoman to UTF-8 (simplified - just use lossy conversion)
            String::from_utf8_lossy(&mdb_data[37..37 + name_length])
                .trim()
                .to_string()
        } else {
            disc_info
                .hfs_mdb
                .as_ref()
                .map(|m| m.volume_name.clone())
                .unwrap_or_else(|| "HFS Volume".to_string())
        };

        // Calculate catalog file offset
        let catalog_offset = partition_offset
            + (alloc_block_start as u64 * 512)
            + (catalog_first_block as u64 * alloc_block_size as u64);

        // Read B-tree header
        let btree_header = reader.read_bytes(catalog_offset, 512)?;

        // Parse node descriptor
        let node_kind = btree_header[8] as i8;
        if node_kind != 1 {
            return Err(FilesystemError::Parse(format!(
                "Expected B-tree header node, got kind {}",
                node_kind
            )));
        }

        // B-tree header record (after 14-byte node descriptor)
        let first_leaf_node = u32::from_be_bytes([
            btree_header[24],
            btree_header[25],
            btree_header[26],
            btree_header[27],
        ]);
        let node_size = u16::from_be_bytes([btree_header[32], btree_header[33]]);

        Ok(Self {
            reader,
            partition_offset,
            alloc_block_size,
            catalog_first_block,
            catalog_extent_length,
            node_size,
            first_leaf_node,
            alloc_block_start,
            volume_name,
        })
    }

    /// Calculate catalog file offset
    fn catalog_offset(&self) -> u64 {
        self.partition_offset
            + (self.alloc_block_start as u64 * 512)
            + (self.catalog_first_block as u64 * self.alloc_block_size as u64)
    }

    /// Read a B-tree node
    fn read_node(&mut self, node_num: u32) -> Result<Vec<u8>, FilesystemError> {
        let offset = self.catalog_offset() + (node_num as u64 * self.node_size as u64);
        self.reader
            .read_bytes(offset, self.node_size as usize)
            .map_err(FilesystemError::from)
    }

    /// List entries in a directory by parent ID
    fn list_directory_by_id(
        &mut self,
        parent_id: u32,
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
                current_node = next_node;
                continue;
            }

            // Process records in this leaf node
            self.process_leaf_node(&node_data, num_records, parent_id, parent_path, &mut entries);

            current_node = next_node;
        }

        // Sort entries
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
        parent_id: u32,
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
            if record_offset + 8 > node_data.len() {
                continue;
            }

            // HFS catalog key format:
            // Byte 0: key length
            // Byte 1: reserved
            // Bytes 2-5: parent ID
            // Byte 6: name length
            // Bytes 7+: name (MacRoman)
            let key_length = node_data[record_offset] as usize;
            if key_length < 6 {
                continue;
            }

            let record_parent_id = u32::from_be_bytes([
                node_data[record_offset + 2],
                node_data[record_offset + 3],
                node_data[record_offset + 4],
                node_data[record_offset + 5],
            ]);

            if record_parent_id != parent_id {
                continue;
            }

            let name_length = node_data[record_offset + 6] as usize;
            if name_length == 0 || name_length > 31 {
                continue; // Thread record or invalid
            }

            let name_start = record_offset + 7;
            let name_end = name_start + name_length;
            if name_end > node_data.len() {
                continue;
            }

            // Convert MacRoman to UTF-8
            let name = String::from_utf8_lossy(&node_data[name_start..name_end]).to_string();

            // Record data starts after key (aligned to even boundary)
            let data_offset = record_offset + 1 + key_length;
            let data_offset = if data_offset % 2 != 0 {
                data_offset + 1
            } else {
                data_offset
            };

            if data_offset + 2 > node_data.len() {
                continue;
            }

            let record_type = node_data[data_offset] as i8;

            let path = if parent_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", parent_path, name)
            };

            match record_type {
                HFS_FOLDER_RECORD => {
                    // Folder record - dir ID at data_offset + 6
                    if data_offset + 10 > node_data.len() {
                        continue;
                    }
                    let dir_id = u32::from_be_bytes([
                        node_data[data_offset + 6],
                        node_data[data_offset + 7],
                        node_data[data_offset + 8],
                        node_data[data_offset + 9],
                    ]);
                    entries.push(FileEntry::new_directory(name, path, dir_id as u64));
                }
                HFS_FILE_RECORD => {
                    // File record - data fork size at data_offset + 62 (physical) or logical at +58
                    if data_offset + 66 > node_data.len() {
                        continue;
                    }
                    let data_size = u32::from_be_bytes([
                        node_data[data_offset + 62],
                        node_data[data_offset + 63],
                        node_data[data_offset + 64],
                        node_data[data_offset + 65],
                    ]);
                    // File ID at data_offset + 20
                    let file_id = u32::from_be_bytes([
                        node_data[data_offset + 20],
                        node_data[data_offset + 21],
                        node_data[data_offset + 22],
                        node_data[data_offset + 23],
                    ]);
                    entries.push(FileEntry::new_file(name, path, data_size as u64, file_id as u64));
                }
                _ => {}
            }
        }
    }
}

impl Filesystem for HfsFilesystem {
    fn root(&mut self) -> Result<FileEntry, FilesystemError> {
        Ok(FileEntry::root(HFS_ROOT_DIR_ID as u64))
    }

    fn list_directory(&mut self, entry: &FileEntry) -> Result<Vec<FileEntry>, FilesystemError> {
        if entry.entry_type != EntryType::Directory {
            return Err(FilesystemError::NotADirectory(entry.path.clone()));
        }

        let dir_id = if entry.path == "/" {
            HFS_ROOT_DIR_ID
        } else {
            entry.location as u32
        };

        self.list_directory_by_id(dir_id, &entry.path)
    }

    fn read_file(&mut self, _entry: &FileEntry) -> Result<Vec<u8>, FilesystemError> {
        // HFS file reading requires finding the file's extent info
        // This is a simplified placeholder - full implementation would
        // traverse the catalog to find extent records
        Err(FilesystemError::Unsupported)
    }

    fn read_file_range(
        &mut self,
        _entry: &FileEntry,
        _offset: u64,
        _length: usize,
    ) -> Result<Vec<u8>, FilesystemError> {
        // Same as read_file
        Err(FilesystemError::Unsupported)
    }

    fn volume_name(&self) -> Option<&str> {
        if self.volume_name.is_empty() {
            None
        } else {
            Some(&self.volume_name)
        }
    }
}
