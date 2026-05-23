//! Multi-disc set detection from filenames.
//!
//! Two filename conventions matter here:
//!
//!   1. Numbered: `(Disc N)`, `(CD N)`, `(Disk N)`, `(DVD N)`, optionally
//!      `... of M`. Standard redump form.
//!   2. Role-named: `(Install Disk)`, `(Game Disc)`, `(Data Disk)`,
//!      `(Bonus Disc)`, `(Extras Disc)`. Used by LucasArts / Sierra /
//!      MicroProse for split installer + runtime pairs where each disc has
//!      a distinct purpose rather than being parts 1/2/3 of one image.
//!
//! Stripping either marker yields a "set key" — files that share the same
//! key are siblings in the same multi-disc release. Used by the bulk-mode
//! auto-apply path and by post-save sibling propagation in single-disc mode.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Matches the numbered disc marker, capturing the number and an optional
/// total count. Mirrors `disc::identifier::DISC_PATTERN` but permits the
/// marker anywhere in the stem (some scene names trail other tags after
/// it). Case-insensitive.
static DISC_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\((Disc|Disk|CD|DVD)\s*(\d+)(?:\s*of\s*(\d+))?[^)]*\)").unwrap()
});

/// Matches a role-named disc marker. Captures the role keyword (Install /
/// Game / Data / Bonus / Extras) so the badge can label it. The trailing
/// `(Disk|Disc)?` makes both `(Install Disk)` and `(Install)` match. The
/// `\b` boundaries keep us from clipping into adjacent words.
static ROLE_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        \s*\(
            \s*(Install|Game|Data|Bonus|Extras)
            (?:\s+(?:Disk|Disc))?
            \s*
        \)
        ",
    )
    .unwrap()
});

/// Total-discs hint from a redump title like "Outlaws (Disc 2 of 3)".
pub fn parse_disc_total(title: &str) -> Option<u32> {
    DISC_MARKER
        .captures(title)
        .and_then(|c| c.get(3))
        .and_then(|m| m.as_str().parse().ok())
}

/// Strip both numbered and role-named disc markers from `stem` so siblings
/// collapse to the same key. Lowercased + whitespace-collapsed so cosmetic
/// variations don't desync siblings.
pub fn set_key(stem: &str) -> String {
    let stripped_numbered = DISC_MARKER.replace_all(stem, "");
    let stripped_both = ROLE_MARKER.replace_all(&stripped_numbered, "");
    stripped_both
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Sibling info: full path + the kind of marker we recognized on it.
#[derive(Debug, Clone)]
pub struct Sibling {
    pub path: PathBuf,
    pub marker: DiscMarker,
}

/// What kind of disc-set marker a filename carries. Numbered discs sort
/// by their number; role discs sort by a stable role order so
/// Install/Game/Data/Bonus/Extras come out in a sensible sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscMarker {
    /// `(Disc N)` / `(CD N)` / etc. — carries a number and optional total.
    Numbered { number: u32, total: Option<u32> },
    /// `(Install Disk)` / `(Game Disk)` / etc. — carries a role keyword.
    /// The string is the canonical capitalization (`Install`, `Game`, …).
    Role(String),
}

impl DiscMarker {
    /// Disc number for numbered markers; `None` for role markers (they
    /// don't have a meaningful position in a sequence).
    pub fn number(&self) -> Option<u32> {
        match self {
            DiscMarker::Numbered { number, .. } => Some(*number),
            DiscMarker::Role(_) => None,
        }
    }

    /// Total-disc hint for numbered markers. Always `None` for role markers.
    pub fn total(&self) -> Option<u32> {
        match self {
            DiscMarker::Numbered { total, .. } => *total,
            DiscMarker::Role(_) => None,
        }
    }

    /// Short label suitable for stamping on artwork — `"Disc 2"`, `"Disc
    /// 2/3"`, `"Install"`, `"Game"`. Disc 1 of a numbered set returns
    /// `None` to match the convention that the first disc isn't badged.
    pub fn badge_label(&self) -> Option<String> {
        match self {
            DiscMarker::Numbered { number, total } => {
                if *number <= 1 {
                    None
                } else {
                    Some(match total {
                        Some(t) if *t > 1 => format!("Disc {number}/{t}"),
                        _ => format!("Disc {number}"),
                    })
                }
            }
            DiscMarker::Role(role) => Some(role.clone()),
        }
    }

    /// Sort key so siblings list in a predictable order.
    fn sort_key(&self) -> (u8, u32, String) {
        match self {
            DiscMarker::Numbered { number, .. } => (0, *number, String::new()),
            DiscMarker::Role(role) => {
                // Install always first (it's typically run first), then the
                // runtime / data discs, then bonus content.
                let pos = match role.as_str() {
                    "Install" => 0,
                    "Game" => 1,
                    "Data" => 2,
                    "Bonus" => 3,
                    "Extras" => 4,
                    _ => 5,
                };
                (1, pos, role.clone())
            }
        }
    }
}

/// Scan the same directory as `disc_path` for files that share its set key
/// and carry a recognized disc marker (numbered or role). The disc itself
/// is excluded. Returns siblings sorted by a stable per-marker order.
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
        let Some(marker) = disc_marker_from_stem(stem) else {
            continue;
        };
        out.push(Sibling { path, marker });
    }

    out.sort_by_key(|s| s.marker.sort_key());
    out
}

/// Recognize either a numbered or role-named disc marker anywhere in
/// `stem`. Numbered takes precedence when both happen to be present.
pub fn disc_marker_from_stem(stem: &str) -> Option<DiscMarker> {
    if let Some(caps) = DISC_MARKER.captures(stem) {
        let number = caps.get(2).and_then(|m| m.as_str().parse().ok())?;
        let total = caps.get(3).and_then(|m| m.as_str().parse().ok());
        return Some(DiscMarker::Numbered { number, total });
    }
    if let Some(caps) = ROLE_MARKER.captures(stem) {
        if let Some(role) = caps.get(1).map(|m| canonical_role(m.as_str())) {
            return Some(DiscMarker::Role(role));
        }
    }
    None
}

/// Extract the disc number from a `(Disc N)`/`(CD N)`/`(Disk N)`/`(DVD N)`
/// marker. Kept for callers that only care about the numbered case.
pub fn disc_number_from_stem(stem: &str) -> Option<u32> {
    match disc_marker_from_stem(stem)? {
        DiscMarker::Numbered { number, .. } => Some(number),
        DiscMarker::Role(_) => None,
    }
}

fn canonical_role(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut chars = lower.chars();
    if let Some(c) = chars.next() {
        out.extend(c.to_uppercase());
    }
    out.push_str(chars.as_str());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_key_collapses_numbered_marker() {
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
    fn set_key_collapses_role_marker() {
        assert_eq!(
            set_key("Klingon Honor Guard (USA) (Install Disk) Mac"),
            set_key("Klingon Honor Guard (USA) (Game Disk) Mac"),
        );
        // Bare-keyword form (no "Disk"/"Disc" suffix) should work too.
        assert_eq!(
            set_key("Foo (USA) (Install)"),
            set_key("Foo (USA) (Game)"),
        );
    }

    #[test]
    fn set_key_unrelated_titles_differ() {
        assert_ne!(set_key("Outlaws (Disc 1)"), set_key("Tomb Raider (Disc 1)"));
        assert_ne!(
            set_key("Outlaws (Install Disk)"),
            set_key("Tomb Raider (Install Disk)"),
        );
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
        assert_eq!(disc_number_from_stem("Game (Install Disk)"), None);
    }

    #[test]
    fn marker_detects_role_variants() {
        let install = disc_marker_from_stem("KHG (USA) (Install Disk) Mac").unwrap();
        assert_eq!(install, DiscMarker::Role("Install".into()));

        let game = disc_marker_from_stem("KHG (USA) (Game Disc) Mac").unwrap();
        assert_eq!(game, DiscMarker::Role("Game".into()));

        let data = disc_marker_from_stem("Foo (Data Disk)").unwrap();
        assert_eq!(data, DiscMarker::Role("Data".into()));

        // Bare keyword: no "Disk" / "Disc" trailer.
        let bare = disc_marker_from_stem("Foo (Bonus)").unwrap();
        assert_eq!(bare, DiscMarker::Role("Bonus".into()));
    }

    #[test]
    fn numbered_takes_precedence_over_role() {
        // Pathological filename that has both — numbered wins.
        let m = disc_marker_from_stem("Foo (Disc 1) (Install Disk)").unwrap();
        assert_eq!(m, DiscMarker::Numbered { number: 1, total: None });
    }

    #[test]
    fn badge_label_skips_disc_one() {
        let m = DiscMarker::Numbered { number: 1, total: Some(3) };
        assert_eq!(m.badge_label(), None);

        let m = DiscMarker::Numbered { number: 2, total: Some(3) };
        assert_eq!(m.badge_label().as_deref(), Some("Disc 2/3"));

        let m = DiscMarker::Numbered { number: 2, total: None };
        assert_eq!(m.badge_label().as_deref(), Some("Disc 2"));
    }

    #[test]
    fn badge_label_for_role_marker() {
        let m = DiscMarker::Role("Install".into());
        assert_eq!(m.badge_label().as_deref(), Some("Install"));
    }

    #[test]
    fn siblings_sorted_install_then_game() {
        // Use the sort key directly since we don't want to hit the filesystem.
        let install = Sibling {
            path: "/foo/x (Install Disk).cue".into(),
            marker: DiscMarker::Role("Install".into()),
        };
        let game = Sibling {
            path: "/foo/x (Game Disk).cue".into(),
            marker: DiscMarker::Role("Game".into()),
        };
        let mut sibs = vec![game.clone(), install.clone()];
        sibs.sort_by_key(|s| s.marker.sort_key());
        assert_eq!(sibs[0].marker, DiscMarker::Role("Install".into()));
        assert_eq!(sibs[1].marker, DiscMarker::Role("Game".into()));
    }
}
