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

/// Mac Roman to Unicode lookup table for bytes 0x80-0xFF.
static MAC_ROMAN_TABLE: [char; 128] = [
    '\u{00C4}', '\u{00C5}', '\u{00C7}', '\u{00C9}', '\u{00D1}', '\u{00D6}', '\u{00DC}', '\u{00E1}',
    '\u{00E0}', '\u{00E2}', '\u{00E4}', '\u{00E3}', '\u{00E5}', '\u{00E7}', '\u{00E9}', '\u{00E8}',
    '\u{00EA}', '\u{00EB}', '\u{00ED}', '\u{00EC}', '\u{00EE}', '\u{00EF}', '\u{00F1}', '\u{00F3}',
    '\u{00F2}', '\u{00F4}', '\u{00F6}', '\u{00F5}', '\u{00FA}', '\u{00F9}', '\u{00FB}', '\u{00FC}',
    '\u{2020}', '\u{00B0}', '\u{00A2}', '\u{00A3}', '\u{00A7}', '\u{2022}', '\u{00B6}', '\u{00DF}',
    '\u{00AE}', '\u{00A9}', '\u{2122}', '\u{00B4}', '\u{00A8}', '\u{2260}', '\u{00C6}', '\u{00D8}',
    '\u{221E}', '\u{00B1}', '\u{2264}', '\u{2265}', '\u{00A5}', '\u{00B5}', '\u{2202}', '\u{2211}',
    '\u{220F}', '\u{03C0}', '\u{222B}', '\u{00AA}', '\u{00BA}', '\u{03A9}', '\u{00E6}', '\u{00F8}',
    '\u{00BF}', '\u{00A1}', '\u{00AC}', '\u{221A}', '\u{0192}', '\u{2248}', '\u{2206}', '\u{00AB}',
    '\u{00BB}', '\u{2026}', '\u{00A0}', '\u{00C0}', '\u{00C3}', '\u{00D5}', '\u{0152}', '\u{0153}',
    '\u{2013}', '\u{2014}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{00F7}', '\u{25CA}',
    '\u{00FF}', '\u{0178}', '\u{2044}', '\u{20AC}', '\u{2039}', '\u{203A}', '\u{FB01}', '\u{FB02}',
    '\u{2021}', '\u{00B7}', '\u{201A}', '\u{201E}', '\u{2030}', '\u{00C2}', '\u{00CA}', '\u{00C1}',
    '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}', '\u{00CC}', '\u{00D3}', '\u{00D4}',
    '\u{F8FF}', '\u{00D2}', '\u{00DA}', '\u{00DB}', '\u{00D9}', '\u{0131}', '\u{02C6}', '\u{02DC}',
    '\u{00AF}', '\u{02D8}', '\u{02D9}', '\u{02DA}', '\u{00B8}', '\u{02DD}', '\u{02DB}', '\u{02C7}',
];

fn mac_roman_to_utf8(data: &[u8]) -> String {
    data.iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                MAC_ROMAN_TABLE[(b - 0x80) as usize]
            }
        })
        .collect()
}

/// HFS extent descriptor: start_block (u16) + block_count (u16).
#[derive(Debug, Clone, Copy)]
struct HfsExtent {
    start_block: u16,
    block_count: u16,
}

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

    /// Find a file's extent descriptors and logical size by CNID.
    ///
    /// HFS file record offsets (from record data start):
    /// - file_id at 20, data logical size at 26, data extents at 74 (3 × 4 bytes).
    fn find_file_extents(
        &mut self,
        target_cnid: u32,
    ) -> Result<(u32, [HfsExtent; 3]), FilesystemError> {
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

            if let Some(result) =
                self.search_node_for_file_extents(&node_data, num_records, target_cnid)
            {
                return Ok(result);
            }

            current_node = next_node;
        }

        Err(FilesystemError::NotFound(format!(
            "File CNID {} not found in HFS catalog",
            target_cnid
        )))
    }

    /// Search a leaf node for a file record with the given CNID.
    fn search_node_for_file_extents(
        &self,
        node_data: &[u8],
        num_records: u16,
        target_cnid: u32,
    ) -> Option<(u32, [HfsExtent; 3])> {
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

            let key_length = node_data[record_offset] as usize;
            if key_length < 6 {
                continue;
            }

            let data_offset = record_offset + 1 + key_length;
            let data_offset = if data_offset % 2 != 0 {
                data_offset + 1
            } else {
                data_offset
            };

            // Need at least 102 bytes for a full HFS file record
            if data_offset + 102 > node_data.len() {
                continue;
            }

            let record_type = node_data[data_offset] as i8;
            if record_type != HFS_FILE_RECORD {
                continue;
            }

            let cnid = u32::from_be_bytes([
                node_data[data_offset + 20],
                node_data[data_offset + 21],
                node_data[data_offset + 22],
                node_data[data_offset + 23],
            ]);

            if cnid != target_cnid {
                continue;
            }

            // Data fork logical size at offset 26
            let logical_size = u32::from_be_bytes([
                node_data[data_offset + 26],
                node_data[data_offset + 27],
                node_data[data_offset + 28],
                node_data[data_offset + 29],
            ]);

            // Data fork extents at offset 74 (3 × HfsExtDescriptor = 3 × 4 bytes)
            let mut extents = [HfsExtent { start_block: 0, block_count: 0 }; 3];
            for j in 0..3 {
                let ext_off = data_offset + 74 + j * 4;
                extents[j] = HfsExtent {
                    start_block: u16::from_be_bytes([
                        node_data[ext_off],
                        node_data[ext_off + 1],
                    ]),
                    block_count: u16::from_be_bytes([
                        node_data[ext_off + 2],
                        node_data[ext_off + 3],
                    ]),
                };
            }

            return Some((logical_size, extents));
        }

        None
    }

    /// Read a byte range from a set of HFS extents.
    ///
    /// `range_offset` and `range_length` are relative to the start of the file's
    /// logical data.  Pass `range_offset = 0` and `range_length = logical_size` to
    /// read the whole file.
    fn read_extents_range(
        &mut self,
        extents: &[HfsExtent; 3],
        logical_size: u32,
        range_offset: u64,
        range_length: usize,
    ) -> Result<Vec<u8>, FilesystemError> {
        let first_alloc_offset = self.partition_offset + self.alloc_block_start as u64 * 512;
        let end = (range_offset + range_length as u64).min(logical_size as u64);

        if range_offset >= end {
            return Ok(Vec::new());
        }

        let mut result = Vec::with_capacity((end - range_offset) as usize);
        let mut logical_pos: u64 = 0;

        for ext in extents {
            if ext.block_count == 0 {
                break;
            }
            let ext_size = ext.block_count as u64 * self.alloc_block_size as u64;
            let ext_end = logical_pos + ext_size;

            if ext_end <= range_offset {
                logical_pos = ext_end;
                continue;
            }
            if logical_pos >= end {
                break;
            }

            let read_start = range_offset.max(logical_pos);
            let read_end = end.min(ext_end);
            let read_len = (read_end - read_start) as usize;
            let offset_in_ext = read_start - logical_pos;

            let physical_offset = first_alloc_offset
                + ext.start_block as u64 * self.alloc_block_size as u64
                + offset_in_ext;

            let chunk = self
                .reader
                .read_bytes(physical_offset, read_len)
                .map_err(FilesystemError::from)?;
            result.extend_from_slice(&chunk);

            logical_pos = ext_end;
        }

        Ok(result)
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
            let name = mac_roman_to_utf8(&node_data[name_start..name_end]);

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
                    // File record must be at least 102 bytes
                    if data_offset + 102 > node_data.len() {
                        continue;
                    }
                    // Data fork logical size at offset 26
                    let data_size = u32::from_be_bytes([
                        node_data[data_offset + 26],
                        node_data[data_offset + 27],
                        node_data[data_offset + 28],
                        node_data[data_offset + 29],
                    ]);
                    // File ID at offset 20
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

    fn read_file(&mut self, entry: &FileEntry) -> Result<Vec<u8>, FilesystemError> {
        if entry.entry_type != EntryType::File {
            return Err(FilesystemError::NotADirectory(format!(
                "{} is not a file",
                entry.path
            )));
        }

        let (logical_size, extents) = self.find_file_extents(entry.location as u32)?;
        self.read_extents_range(&extents, logical_size, 0, logical_size as usize)
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

        let (logical_size, extents) = self.find_file_extents(entry.location as u32)?;
        let actual_length =
            std::cmp::min(length as u64, (logical_size as u64).saturating_sub(offset)) as usize;
        if actual_length == 0 {
            return Ok(Vec::new());
        }
        self.read_extents_range(&extents, logical_size, offset, actual_length)
    }

    fn volume_name(&self) -> Option<&str> {
        if self.volume_name.is_empty() {
            None
        } else {
            Some(&self.volume_name)
        }
    }
}
