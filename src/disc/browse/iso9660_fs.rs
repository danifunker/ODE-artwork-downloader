//! ISO 9660 filesystem implementation for directory listing and file reading

use super::entry::{EntryType, FileEntry};
use super::filesystem::{Filesystem, FilesystemError};
use super::reader::{SectorReader, SECTOR_SIZE};

/// ISO 9660 filesystem implementation
pub struct Iso9660Filesystem {
    reader: Box<dyn SectorReader>,
    /// Location of root directory (LBA)
    root_location: u32,
    /// Size of root directory in bytes
    root_size: u32,
    /// Volume identifier
    volume_id: String,
}

/// ISO 9660 Directory Record
#[derive(Debug, Clone)]
struct DirectoryRecord {
    /// Length of directory record
    length: u8,
    /// Location of extent (LBA)
    extent_location: u32,
    /// Data length (file size)
    data_length: u32,
    /// File flags
    file_flags: u8,
    /// File identifier (name)
    file_identifier: String,
}

impl DirectoryRecord {
    /// Check if this is a directory
    fn is_directory(&self) -> bool {
        (self.file_flags & 0x02) != 0
    }

    /// Check if this is the "." entry
    fn is_self(&self) -> bool {
        self.file_identifier == "\0" || self.file_identifier.is_empty()
    }

    /// Check if this is the ".." entry
    fn is_parent(&self) -> bool {
        self.file_identifier == "\x01"
    }

    /// Get clean filename (without version suffix)
    fn clean_name(&self) -> String {
        if self.is_self() {
            return ".".to_string();
        }
        if self.is_parent() {
            return "..".to_string();
        }

        let name = &self.file_identifier;

        // Remove version suffix (;1)
        let name = if let Some(idx) = name.rfind(';') {
            &name[..idx]
        } else {
            name
        };

        // Remove trailing dot for directories
        let name = if self.is_directory() {
            name.trim_end_matches('.')
        } else {
            name
        };

        name.to_string()
    }
}

impl Iso9660Filesystem {
    /// Create a new ISO 9660 filesystem from a sector reader
    pub fn new(mut reader: Box<dyn SectorReader>) -> Result<Self, FilesystemError> {
        // Read the Primary Volume Descriptor at sector 16
        let pvd_sector = reader.read_sector(16)?;

        // Validate PVD
        if pvd_sector[0] != 1 {
            return Err(FilesystemError::Parse(format!(
                "Not a Primary Volume Descriptor (type {})",
                pvd_sector[0]
            )));
        }

        if &pvd_sector[1..6] != b"CD001" {
            return Err(FilesystemError::Parse(
                "Invalid ISO 9660 identifier".to_string(),
            ));
        }

        // Extract volume ID (bytes 40-71)
        let volume_id = extract_string(&pvd_sector[40..72]);

        // Extract root directory record (bytes 156-189)
        let root_record = &pvd_sector[156..190];

        // Root directory location (bytes 2-5, little-endian)
        let root_location = u32::from_le_bytes([
            root_record[2],
            root_record[3],
            root_record[4],
            root_record[5],
        ]);

        // Root directory size (bytes 10-13, little-endian)
        let root_size = u32::from_le_bytes([
            root_record[10],
            root_record[11],
            root_record[12],
            root_record[13],
        ]);

        Ok(Self {
            reader,
            root_location,
            root_size,
            volume_id,
        })
    }

    /// Parse directory records from raw directory data
    fn parse_directory(&self, data: &[u8], parent_path: &str) -> Vec<FileEntry> {
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            // Record length is first byte
            let record_length = data[offset] as usize;

            // Length 0 means end of sector, skip to next sector boundary
            if record_length == 0 {
                let next_sector = ((offset / SECTOR_SIZE as usize) + 1) * SECTOR_SIZE as usize;
                if next_sector >= data.len() {
                    break;
                }
                offset = next_sector;
                continue;
            }

            // Ensure we have enough data
            if offset + record_length > data.len() {
                break;
            }

            let record_data = &data[offset..offset + record_length];

            if let Some(record) = self.parse_directory_record(record_data) {
                // Skip . and .. entries
                if !record.is_self() && !record.is_parent() {
                    let name = record.clean_name();
                    let path = if parent_path == "/" {
                        format!("/{}", name)
                    } else {
                        format!("{}/{}", parent_path, name)
                    };

                    let entry = if record.is_directory() {
                        FileEntry::new_directory(name, path, record.extent_location as u64)
                    } else {
                        FileEntry::new_file(
                            name,
                            path,
                            record.data_length as u64,
                            record.extent_location as u64,
                        )
                    };

                    entries.push(entry);
                }
            }

            offset += record_length;
        }

        // Sort entries: directories first, then by name
        entries.sort_by(|a, b| {
            match (a.entry_type, b.entry_type) {
                (EntryType::Directory, EntryType::File) => std::cmp::Ordering::Less,
                (EntryType::File, EntryType::Directory) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });

        entries
    }

    /// Parse a single directory record
    fn parse_directory_record(&self, data: &[u8]) -> Option<DirectoryRecord> {
        if data.len() < 33 {
            return None;
        }

        let length = data[0];
        if length == 0 {
            return None;
        }

        // Extended attribute record length
        let _ext_attr_length = data[1];

        // Location of extent (LBA) - little-endian at bytes 2-5
        let extent_location =
            u32::from_le_bytes([data[2], data[3], data[4], data[5]]);

        // Data length - little-endian at bytes 10-13
        let data_length =
            u32::from_le_bytes([data[10], data[11], data[12], data[13]]);

        // File flags at byte 25
        let file_flags = data[25];

        // File identifier length at byte 32
        let identifier_length = data[32] as usize;

        // File identifier starts at byte 33
        if data.len() < 33 + identifier_length {
            return None;
        }

        let identifier_bytes = &data[33..33 + identifier_length];
        let file_identifier = String::from_utf8_lossy(identifier_bytes).to_string();

        Some(DirectoryRecord {
            length,
            extent_location,
            data_length,
            file_flags,
            file_identifier,
        })
    }

    /// Read directory data from the disc
    fn read_directory_data(&mut self, location: u32, size: u32) -> Result<Vec<u8>, FilesystemError> {
        let sector_count = (size as u64 + SECTOR_SIZE - 1) / SECTOR_SIZE;
        let data = self.reader.read_sectors(location as u64, sector_count)?;
        Ok(data[..size as usize].to_vec())
    }
}

impl Filesystem for Iso9660Filesystem {
    fn root(&mut self) -> Result<FileEntry, FilesystemError> {
        let mut root = FileEntry::root(self.root_location as u64);
        root.size = self.root_size as u64;
        Ok(root)
    }

    fn list_directory(&mut self, entry: &FileEntry) -> Result<Vec<FileEntry>, FilesystemError> {
        if entry.entry_type != EntryType::Directory {
            return Err(FilesystemError::NotADirectory(entry.path.clone()));
        }

        // For root, use stored root_size; for others, we need to read the directory record
        let (location, size) = if entry.path == "/" {
            (self.root_location, self.root_size)
        } else {
            // Read the directory's extent
            // The size should be stored in the entry, but we don't have it
            // For now, read a reasonable amount and parse what we can
            // In a complete implementation, we'd store the size in the entry
            let sector = self.reader.read_sector(entry.location)?;

            // Parse the first record to get the directory's own info
            if let Some(record) = self.parse_directory_record(&sector) {
                (entry.location as u32, record.data_length)
            } else {
                // Fallback: read up to 64KB
                (entry.location as u32, 65536)
            }
        };

        let data = self.read_directory_data(location, size)?;
        let entries = self.parse_directory(&data, &entry.path);

        Ok(entries)
    }

    fn read_file(&mut self, entry: &FileEntry) -> Result<Vec<u8>, FilesystemError> {
        if entry.entry_type != EntryType::File {
            return Err(FilesystemError::NotADirectory(format!(
                "{} is not a file",
                entry.path
            )));
        }

        let sector_count = (entry.size + SECTOR_SIZE - 1) / SECTOR_SIZE;
        let data = self.reader.read_sectors(entry.location, sector_count)?;

        Ok(data[..entry.size as usize].to_vec())
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

        // Clamp length to file size
        let actual_length = std::cmp::min(length as u64, entry.size.saturating_sub(offset)) as usize;

        if actual_length == 0 {
            return Ok(Vec::new());
        }

        // Calculate which sectors we need
        let file_start_lba = entry.location;
        let start_sector = offset / SECTOR_SIZE;
        let end_sector = (offset + actual_length as u64 + SECTOR_SIZE - 1) / SECTOR_SIZE;
        let sector_count = end_sector - start_sector;

        let data = self.reader.read_sectors(file_start_lba + start_sector, sector_count)?;

        let offset_in_first_sector = (offset % SECTOR_SIZE) as usize;
        Ok(data[offset_in_first_sector..offset_in_first_sector + actual_length].to_vec())
    }

    fn volume_name(&self) -> Option<&str> {
        if self.volume_id.is_empty() {
            None
        } else {
            Some(&self.volume_id)
        }
    }
}

/// Extract a string from a byte slice, trimming trailing spaces and nulls
fn extract_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(|c: char| c == ' ' || c == '\0')
        .to_string()
}
