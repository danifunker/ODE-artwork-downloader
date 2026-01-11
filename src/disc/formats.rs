//! Disc image format and filesystem type definitions

use std::path::Path;

/// Supported disc image formats
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DiscFormat {
    /// ISO 9660 disc image (.iso)
    Iso,
    /// BIN/CUE format (raw binary with cue sheet)
    BinCue,
    /// MAME Compressed Hunks of Data (.chd)
    Chd,
    /// Media Descriptor Sidecar / Media Data File (.mds/.mdf)
    MdsMdf,
}

impl DiscFormat {
    /// Detect disc format from file extension
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "iso" | "toast" => Some(Self::Iso),
            "bin" | "cue" => Some(Self::BinCue),
            "chd" => Some(Self::Chd),
            "mds" | "mdf" => Some(Self::MdsMdf),
            _ => None,
        }
    }

    /// Get the display name for this format
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Iso => "ISO 9660",
            Self::BinCue => "BIN/CUE",
            Self::Chd => "CHD (Compressed Hunks of Data)",
            Self::MdsMdf => "MDS/MDF",
        }
    }

    /// Get supported file extensions for this format
    #[allow(dead_code)]
    pub fn extensions(&self) -> &'static [&'static str] {
        match self {
            Self::Iso => &["iso", "toast"],
            Self::BinCue => &["bin", "cue"],
            Self::Chd => &["chd"],
            Self::MdsMdf => &["mds", "mdf"],
        }
    }
}

/// Supported filesystem types found on disc images
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FilesystemType {
    /// ISO 9660 standard filesystem
    Iso9660,
    /// Joliet extensions (Unicode support)
    Joliet,
    /// Universal Disk Format
    Udf,
    /// Hierarchical File System (classic Mac)
    Hfs,
    /// HFS+ (Mac OS Extended)
    HfsPlus,
    /// Unknown or unsupported filesystem
    Unknown,
}

impl FilesystemType {
    /// Get the display name for this filesystem type
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Iso9660 => "ISO 9660",
            Self::Joliet => "Joliet",
            Self::Udf => "UDF",
            Self::Hfs => "HFS",
            Self::HfsPlus => "HFS+",
            Self::Unknown => "Unknown",
        }
    }
}

/// Get all supported file extensions for file dialogs
pub fn supported_extensions() -> Vec<&'static str> {
    vec!["iso", "toast", "bin", "cue", "chd", "mds", "mdf"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_detection() {
        assert_eq!(DiscFormat::from_path(Path::new("game.iso")), Some(DiscFormat::Iso));
        assert_eq!(DiscFormat::from_path(Path::new("game.ISO")), Some(DiscFormat::Iso));
        assert_eq!(DiscFormat::from_path(Path::new("game.chd")), Some(DiscFormat::Chd));
        assert_eq!(DiscFormat::from_path(Path::new("game.bin")), Some(DiscFormat::BinCue));
        assert_eq!(DiscFormat::from_path(Path::new("game.cue")), Some(DiscFormat::BinCue));
        assert_eq!(DiscFormat::from_path(Path::new("game.txt")), None);
    }
}
