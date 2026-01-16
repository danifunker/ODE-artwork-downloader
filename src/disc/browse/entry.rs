//! File entry structures for disc filesystem browsing

/// Represents a single file or directory entry in a disc filesystem
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// File/directory name
    pub name: String,
    /// Full path from root (e.g., "/System/Library/file.txt")
    pub path: String,
    /// Entry type (file or directory)
    pub entry_type: EntryType,
    /// File size in bytes (0 for directories)
    pub size: u64,
    /// Starting location - interpretation depends on filesystem
    /// ISO 9660: LBA (Logical Block Address)
    /// HFS/HFS+: Extent start or CNID
    pub location: u64,
    /// Children entries (only populated for directories when expanded)
    pub children: Option<Vec<FileEntry>>,
}

/// Type of filesystem entry
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Directory,
}

impl FileEntry {
    /// Create a new file entry
    pub fn new_file(name: String, path: String, size: u64, location: u64) -> Self {
        Self {
            name,
            path,
            entry_type: EntryType::File,
            size,
            location,
            children: None,
        }
    }

    /// Create a new directory entry
    pub fn new_directory(name: String, path: String, location: u64) -> Self {
        Self {
            name,
            path,
            entry_type: EntryType::Directory,
            size: 0,
            location,
            children: None,
        }
    }

    /// Create root directory entry
    pub fn root(location: u64) -> Self {
        Self {
            name: String::new(),
            path: "/".to_string(),
            entry_type: EntryType::Directory,
            size: 0,
            location,
            children: None,
        }
    }

    /// Check if this is a directory
    pub fn is_directory(&self) -> bool {
        self.entry_type == EntryType::Directory
    }

    /// Check if this is a file
    pub fn is_file(&self) -> bool {
        self.entry_type == EntryType::File
    }

    /// Get a display-friendly size string
    pub fn size_string(&self) -> String {
        if self.is_directory() {
            return String::new();
        }

        if self.size < 1024 {
            format!("{} B", self.size)
        } else if self.size < 1024 * 1024 {
            format!("{:.1} KB", self.size as f64 / 1024.0)
        } else if self.size < 1024 * 1024 * 1024 {
            format!("{:.1} MB", self.size as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.2} GB", self.size as f64 / (1024.0 * 1024.0 * 1024.0))
        }
    }
}
