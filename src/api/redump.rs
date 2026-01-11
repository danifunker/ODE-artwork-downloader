//! Redump DAT file parsing
//!
//! Parses Redump XML DAT files for game identification.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use quick_xml::de::from_reader;
use serde::Deserialize;

/// A ROM entry in the DAT file
#[derive(Debug, Clone, Deserialize)]
pub struct Rom {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@size")]
    pub size: Option<String>,
    #[serde(rename = "@crc")]
    pub crc: Option<String>,
    #[serde(rename = "@md5")]
    pub md5: Option<String>,
    #[serde(rename = "@sha1")]
    pub sha1: Option<String>,
}

/// A game entry in the DAT file
#[derive(Debug, Clone, Deserialize)]
pub struct Game {
    #[serde(rename = "@name")]
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "rom", default)]
    pub roms: Vec<Rom>,
}

/// DAT file header
#[derive(Debug, Clone, Deserialize)]
pub struct Header {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
}

/// Root datafile structure
#[derive(Debug, Clone, Deserialize)]
pub struct Datafile {
    pub header: Option<Header>,
    #[serde(rename = "game", default)]
    pub games: Vec<Game>,
}

/// A simplified game entry for matching
#[derive(Debug, Clone)]
pub struct RedumpGame {
    /// Game name (e.g., "Batman Forever (Europe)")
    pub name: String,
    /// Category (e.g., "Games")
    pub category: Option<String>,
    /// List of filenames associated with this game
    pub filenames: Vec<String>,
    /// CRC checksums for the ROMs
    pub crcs: Vec<String>,
}

impl From<Game> for RedumpGame {
    fn from(game: Game) -> Self {
        let filenames: Vec<String> = game.roms.iter()
            .map(|r| r.name.clone())
            .collect();
        let crcs: Vec<String> = game.roms.iter()
            .filter_map(|r| r.crc.clone())
            .collect();

        RedumpGame {
            name: game.name,
            category: game.category,
            filenames,
            crcs,
        }
    }
}

/// Redump database loaded from DAT files
#[derive(Debug, Default)]
pub struct RedumpDatabase {
    /// All games indexed by lowercase base filename (without extension)
    filename_index: HashMap<String, Vec<RedumpGame>>,
    /// All games for fuzzy matching
    all_games: Vec<RedumpGame>,
    /// System name (e.g., "IBM - PC compatible")
    pub system_name: Option<String>,
}

impl RedumpDatabase {
    /// Create a new empty database
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a DAT file (supports .dat XML files and .zip containing a .dat)
    pub fn load_dat<P: AsRef<Path>>(&mut self, path: P) -> Result<usize, String> {
        let path = path.as_ref();

        if path.extension().map(|e| e == "zip").unwrap_or(false) {
            self.load_zip(path)
        } else {
            self.load_xml(path)
        }
    }

    /// Load a .zip file containing a DAT
    fn load_zip(&mut self, path: &Path) -> Result<usize, String> {
        let file = File::open(path)
            .map_err(|e| format!("Failed to open ZIP: {}", e))?;

        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Failed to read ZIP: {}", e))?;

        // Find the .dat file in the archive
        for i in 0..archive.len() {
            let file = archive.by_index(i)
                .map_err(|e| format!("Failed to read ZIP entry: {}", e))?;

            if file.name().ends_with(".dat") {
                log::debug!("Found DAT file in ZIP: {}", file.name());
                return self.parse_xml_reader(BufReader::new(file));
            }
        }

        Err("No .dat file found in ZIP archive".to_string())
    }

    /// Load an XML .dat file directly
    fn load_xml(&mut self, path: &Path) -> Result<usize, String> {
        let file = File::open(path)
            .map_err(|e| format!("Failed to open DAT: {}", e))?;

        self.parse_xml_reader(BufReader::new(file))
    }

    /// Parse XML from a reader
    fn parse_xml_reader<R: std::io::BufRead>(&mut self, reader: R) -> Result<usize, String> {
        let datafile: Datafile = from_reader(reader)
            .map_err(|e| format!("Failed to parse DAT XML: {}", e))?;

        if let Some(ref header) = datafile.header {
            self.system_name = header.name.clone();
            log::info!("Loaded DAT for: {:?}", header.name);
        }

        let count = datafile.games.len();

        for game in datafile.games {
            let redump_game = RedumpGame::from(game);

            // Index by each filename's base name (without extension)
            for filename in &redump_game.filenames {
                let base = Path::new(filename)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(filename)
                    .to_lowercase();

                self.filename_index
                    .entry(base)
                    .or_insert_with(Vec::new)
                    .push(redump_game.clone());
            }

            self.all_games.push(redump_game);
        }

        Ok(count)
    }

    /// Find games by filename (case-insensitive, matches base name)
    pub fn find_by_filename(&self, filename: &str) -> Vec<&RedumpGame> {
        let base = Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(filename)
            .to_lowercase();

        self.filename_index
            .get(&base)
            .map(|games| games.iter().collect())
            .unwrap_or_default()
    }

    /// Find games by fuzzy matching on the name
    /// Returns games sorted by match quality (best first)
    pub fn find_by_name_fuzzy(&self, query: &str, max_results: usize) -> Vec<(&RedumpGame, f64)> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut results: Vec<(&RedumpGame, f64)> = self.all_games.iter()
            .filter_map(|game| {
                let name_lower = game.name.to_lowercase();

                // Calculate a simple match score
                let score = calculate_match_score(&query_lower, &query_words, &name_lower);

                if score > 0.3 {
                    Some((game, score))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(max_results);

        results
    }

    /// Get total number of games in the database
    pub fn game_count(&self) -> usize {
        self.all_games.len()
    }
}

/// Calculate a fuzzy match score between a query and a game name
fn calculate_match_score(query: &str, query_words: &[&str], name: &str) -> f64 {
    // Exact match
    if query == name {
        return 1.0;
    }

    // Check if query is contained in name
    if name.contains(query) {
        return 0.9;
    }

    // Check if name starts with query
    if name.starts_with(query) {
        return 0.85;
    }

    // Word-based matching
    let name_words: Vec<&str> = name.split_whitespace().collect();
    let mut matched_words = 0;

    for qw in query_words {
        if name_words.iter().any(|nw| nw.contains(qw) || qw.contains(nw)) {
            matched_words += 1;
        }
    }

    if query_words.is_empty() {
        return 0.0;
    }

    let word_score = matched_words as f64 / query_words.len() as f64;

    // Bonus for matching at the start
    if !query_words.is_empty() && !name_words.is_empty() {
        if name_words[0].starts_with(query_words[0]) {
            return word_score * 0.8 + 0.1;
        }
    }

    word_score * 0.7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_score() {
        let query = "batman forever";
        let query_words: Vec<&str> = query.split_whitespace().collect();

        let score1 = calculate_match_score(query, &query_words, "batman forever (europe)");
        let score2 = calculate_match_score(query, &query_words, "batman forever (usa)");
        let score3 = calculate_match_score(query, &query_words, "superman");

        assert!(score1 > 0.8);
        assert!(score2 > 0.8);
        assert!(score3 < 0.3);
    }
}
