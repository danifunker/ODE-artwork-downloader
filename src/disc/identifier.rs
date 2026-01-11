//! Game identification from disc images and filenames
//!
//! Provides functionality to extract game titles from:
//! - Disc image volume labels
//! - Filename parsing with fuzzy logic

use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// Confidence level for game identification
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfidenceLevel {
    /// Low confidence - derived from filename only
    Low,
    /// Medium confidence - derived from CHD metadata or partial volume info
    Medium,
    /// High confidence - derived from clean volume label
    High,
}

impl ConfidenceLevel {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Low => "Low (filename)",
            Self::Medium => "Medium (metadata)",
            Self::High => "High (volume label)",
        }
    }
}

/// Result of parsing a filename for game information
#[derive(Debug, Clone)]
pub struct ParsedFilename {
    /// Cleaned game title
    pub title: String,
    /// Original filename without extension
    pub original: String,
    /// Detected region code (if any)
    pub region: Option<String>,
    /// Detected disc number (if any)
    pub disc_number: Option<u32>,
    /// Detected serial number (if any)
    pub serial: Option<String>,
    /// Detected version info (if any)
    pub version: Option<String>,
}

// Regex patterns for filename parsing
static REGION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\((USA|Europe|Japan|World|En|Fr|De|Es|It|Ja|Ko|Zh|PAL|NTSC|NTSC-U|NTSC-J|PAL-E)[^)]*\)").unwrap()
});

static DISC_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\((Disc|CD|DVD|Disk)\s*(\d+)[^)]*\)").unwrap()
});

static SERIAL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*\[([A-Z]{2,4}[PS]?-?\d{4,6})\]").unwrap()
});

static VERSION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\((v\d+\.?\d*|Rev\s*[A-Z0-9]+)\)").unwrap()
});

static EXTRA_TAGS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*\([^)]+\)|\s*\[[^\]]+\]").unwrap()
});

/// Parse a filename to extract game information
///
/// # Arguments
/// * `path` - Path to the disc image file
///
/// # Returns
/// * `ParsedFilename` - Extracted information from the filename
pub fn parse_filename(path: &Path) -> ParsedFilename {
    let original = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let mut title = original.clone();
    let mut region = None;
    let mut disc_number = None;
    let mut serial = None;
    let mut version = None;

    // Extract region
    if let Some(caps) = REGION_PATTERN.captures(&title) {
        region = caps.get(1).map(|m| m.as_str().to_string());
        title = REGION_PATTERN.replace(&title, "").to_string();
    }

    // Extract disc number
    if let Some(caps) = DISC_PATTERN.captures(&title) {
        disc_number = caps.get(2).and_then(|m| m.as_str().parse().ok());
        title = DISC_PATTERN.replace(&title, "").to_string();
    }

    // Extract serial number
    if let Some(caps) = SERIAL_PATTERN.captures(&title) {
        serial = caps.get(1).map(|m| m.as_str().to_string());
        title = SERIAL_PATTERN.replace(&title, "").to_string();
    }

    // Extract version
    if let Some(caps) = VERSION_PATTERN.captures(&title) {
        version = caps.get(1).map(|m| m.as_str().to_string());
        title = VERSION_PATTERN.replace(&title, "").to_string();
    }

    // Remove any remaining tags in parentheses or brackets
    title = EXTRA_TAGS_PATTERN.replace_all(&title, "").to_string();

    // Normalize separators
    title = title
        .replace('_', " ")
        .replace('-', " ")
        .replace('.', " ");

    // Clean up whitespace
    title = title.split_whitespace().collect::<Vec<_>>().join(" ");

    ParsedFilename {
        title,
        original,
        region,
        disc_number,
        serial,
        version,
    }
}

/// Normalize a volume label for display/search
///
/// Cleans up common volume label formatting issues.
pub fn normalize_volume_label(label: &str) -> String {
    let mut result = label.to_string();

    // Replace underscores with spaces
    result = result.replace('_', " ");

    // Clean up whitespace
    result = result.split_whitespace().collect::<Vec<_>>().join(" ");

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_filename() {
        let path = Path::new("Final Fantasy VII.iso");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Final Fantasy VII");
        assert_eq!(parsed.region, None);
        assert_eq!(parsed.disc_number, None);
    }

    #[test]
    fn test_parse_filename_with_region() {
        let path = Path::new("Final Fantasy VII (USA).iso");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Final Fantasy VII");
        assert_eq!(parsed.region, Some("USA".to_string()));
    }

    #[test]
    fn test_parse_filename_with_disc() {
        let path = Path::new("Final Fantasy VII (USA) (Disc 1).iso");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Final Fantasy VII");
        assert_eq!(parsed.region, Some("USA".to_string()));
        assert_eq!(parsed.disc_number, Some(1));
    }

    #[test]
    fn test_parse_filename_with_serial() {
        let path = Path::new("Final Fantasy VII [SCUS-94163].iso");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Final Fantasy VII");
        assert_eq!(parsed.serial, Some("SCUS-94163".to_string()));
    }

    #[test]
    fn test_parse_filename_with_version() {
        let path = Path::new("Game Title (USA) (v1.1).iso");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Game Title");
        assert_eq!(parsed.version, Some("v1.1".to_string()));
    }

    #[test]
    fn test_parse_filename_with_underscores() {
        let path = Path::new("Final_Fantasy_VII.iso");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Final Fantasy VII");
    }

    #[test]
    fn test_parse_complex_filename() {
        let path = Path::new("Final Fantasy VII (USA) (Disc 1) [SCUS-94163] (v1.0).chd");
        let parsed = parse_filename(path);

        assert_eq!(parsed.title, "Final Fantasy VII");
        assert_eq!(parsed.region, Some("USA".to_string()));
        assert_eq!(parsed.disc_number, Some(1));
        assert_eq!(parsed.serial, Some("SCUS-94163".to_string()));
        assert_eq!(parsed.version, Some("v1.0".to_string()));
    }

    #[test]
    fn test_normalize_volume_label() {
        assert_eq!(normalize_volume_label("GAME_TITLE"), "GAME TITLE");
        assert_eq!(normalize_volume_label("  GAME   TITLE  "), "GAME TITLE");
    }
}
