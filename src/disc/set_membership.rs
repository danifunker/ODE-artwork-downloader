//! Multi-disc set detection from filenames.
//!
//! The redump-style naming convention puts a `(Disc N)` / `(CD N)` /
//! `(Disk N)` marker in the filename for each disc of a set. Stripping that
//! marker yields a "set key" — files that share the same key are siblings
//! in the same multi-disc release.
//!
//! Used by the bulk-mode auto-apply path and by post-save sibling propagation
//! in single-disc mode.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Matches the disc marker inside a stem, capturing the number.
/// Mirrors the production parser in `disc::identifier::DISC_PATTERN` but
/// permits the marker anywhere in the stem (some scene names trail other
/// tags after it). Case-insensitive.
static DISC_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\((Disc|Disk|CD|DVD)\s*(\d+)(?:\s*of\s*(\d+))?[^)]*\)").unwrap()
});

/// Total-discs hint from a redump title like "Outlaws (Disc 2 of 3)".
pub fn parse_disc_total(title: &str) -> Option<u32> {
    DISC_MARKER
        .captures(title)
        .and_then(|c| c.get(3))
        .and_then(|m| m.as_str().parse().ok())
}

/// Strip the disc marker from `stem` so two siblings collapse to the same
/// key. Returns the lowercased, trimmed remainder; collapses internal runs
/// of whitespace so cosmetic variations don't desync siblings.
pub fn set_key(stem: &str) -> String {
    let without = DISC_MARKER.replace_all(stem, "").to_string();
    let collapsed: String = without
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.to_ascii_lowercase()
}

/// Sibling info: full path + the disc number we parsed from its filename.
#[derive(Debug, Clone)]
pub struct Sibling {
    pub path: PathBuf,
    pub disc_number: u32,
}

/// Scan the same directory as `disc_path` for files that share its set key
/// and have a disc marker. The disc itself is excluded. Returns siblings
/// sorted by disc number; missing numbers in a series (e.g. only 1 and 3
/// present) are reported as-is rather than padded.
pub fn siblings_in_dir(disc_path: &Path) -> Vec<Sibling> {
    let Some(my_stem) = disc_path.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let Some(dir) = disc_path.parent() else {
        return Vec::new();
    };
    let key = set_key(my_stem);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out: Vec<Sibling> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == disc_path {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if set_key(stem) != key {
            continue;
        }
        let Some(num) = disc_number_from_stem(stem) else {
            continue;
        };
        out.push(Sibling {
            path,
            disc_number: num,
        });
    }

    out.sort_by_key(|s| s.disc_number);
    out
}

/// Extract the disc number from a `(Disc N)`/`(CD N)`/`(Disk N)`/`(DVD N)`
/// marker anywhere in `stem`. None if no marker is present.
pub fn disc_number_from_stem(stem: &str) -> Option<u32> {
    DISC_MARKER
        .captures(stem)
        .and_then(|c| c.get(2))
        .and_then(|m| m.as_str().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_key_collapses_disc_marker() {
        assert_eq!(
            set_key("Outlaws (USA) (Disc 1)"),
            set_key("Outlaws (USA) (Disc 2)"),
        );
        assert_eq!(
            set_key("Outlaws (Disc 1 of 2)"),
            set_key("Outlaws (Disc 2 of 2)"),
        );
    }

    #[test]
    fn set_key_unrelated_titles_differ() {
        assert_ne!(set_key("Outlaws (Disc 1)"), set_key("Tomb Raider (Disc 1)"));
    }

    #[test]
    fn parse_total_from_redump_title() {
        assert_eq!(parse_disc_total("Outlaws (Disc 2 of 3)"), Some(3));
        assert_eq!(parse_disc_total("Outlaws (Disc 2)"), None);
        assert_eq!(parse_disc_total("Outlaws"), None);
    }

    #[test]
    fn disc_number_extraction_handles_variants() {
        assert_eq!(disc_number_from_stem("Game (Disc 2)"), Some(2));
        assert_eq!(disc_number_from_stem("Game (CD 3)"), Some(3));
        assert_eq!(disc_number_from_stem("Game (Disk 1 of 4)"), Some(1));
        assert_eq!(disc_number_from_stem("Game"), None);
    }
}
