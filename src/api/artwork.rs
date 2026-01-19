//! Artwork search functionality
//!
//! Provides cover art search using web image searches based on parsed filename information.

use crate::disc::{DiscInfo, ParsedFilename};
use std::path::Path;
use regex::Regex;
use std::sync::LazyLock;

// Regex for detecting CamelCase or PascalCase
static CAMEL_CASE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Match lowercase followed by uppercase, or multiple uppercase followed by lowercase
    Regex::new(r"([a-z])([A-Z])|([A-Z]+)([A-Z][a-z])").unwrap()
});

/// Configuration for search behavior
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Sites to exclude from search results
    pub exclusion_sites: Vec<String>,
    /// Platform keywords to exclude from search results
    pub exclusion_platforms: Vec<String>,
    /// Keywords to use for CD searches
    pub cd_keywords: Vec<String>,
    /// Keywords to use for DVD searches
    pub dvd_keywords: Vec<String>,
    /// Known publishers to exclude from game name search
    pub known_publishers: Vec<String>,
    /// Content type for site selection
    pub content_type: ContentType,
    /// Known sites for games
    pub games_sites: Vec<String>,
    /// Known sites for apps and utilities
    pub apps_sites: Vec<String>,
    /// Known sites for audio CDs
    pub audio_sites: Vec<String>,
    /// Custom user agent string for HTTP requests
    pub user_agent: Option<String>,
}

/// Content type for different disc categories
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Any,
    Games,
    AppsUtilities,
    AudioCDs,
}

impl ContentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentType::Any => "any",
            ContentType::Games => "games",
            ContentType::AppsUtilities => "apps_utilities",
            ContentType::AudioCDs => "audio_cds",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "games" => ContentType::Games,
            "apps_utilities" | "apps" => ContentType::AppsUtilities,
            "audio_cds" | "audio" => ContentType::AudioCDs,
            _ => ContentType::Any, // default
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ContentType::Any => "Any",
            ContentType::Games => "Games",
            ContentType::AppsUtilities => "Apps & Utilities",
            ContentType::AudioCDs => "Audio CDs",
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        // Try to load from config.json
        if let Ok(config_str) = std::fs::read_to_string("config.json") {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&config_str) {
                if let Some(search) = json.get("search") {
                    return Self {
                        exclusion_sites: search.get("exclusion_sites")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_else(|| vec!["ebay.com".to_string()]),
                        
                        exclusion_platforms: search.get("exclusion_platforms")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_else(|| vec!["playstation".to_string(), "xbox".to_string(), "nintendo".to_string()]),
                        
                        cd_keywords: search.get("cd_keywords")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_else(|| vec!["CD".to_string(), "jewel case".to_string()]),
                        
                        dvd_keywords: search.get("dvd_keywords")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_else(|| vec!["DVD".to_string()]),
                        
                        content_type: search.get("content_type")
                            .and_then(|v| v.as_str())
                            .map(ContentType::from_str)
                            .unwrap_or(ContentType::Any),
                        
                        games_sites: search.get("known_sites")
                            .and_then(|v| v.get("games"))
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default(),
                        
                        apps_sites: search.get("known_sites")
                            .and_then(|v| v.get("apps_utilities"))
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default(),
                        
                        audio_sites: search.get("known_sites")
                            .and_then(|v| v.get("audio_cds"))
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default(),
                        
                        known_publishers: search.get("known_publishers")
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                            .unwrap_or_default(),

                        user_agent: search.get("user_agent")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    };
                }
            }
        }

        // Fallback if config.json doesn't exist or can't be read
        Self {
            exclusion_sites: vec!["ebay.com".to_string()],
            exclusion_platforms: vec![
                "playstation".to_string(),
                "xbox".to_string(),
                "nintendo".to_string(),
            ],
            cd_keywords: vec!["CD".to_string(), "jewel case".to_string()],
            dvd_keywords: vec!["DVD".to_string()],
            content_type: ContentType::Any,
            games_sites: Vec::new(),
            apps_sites: Vec::new(),
            audio_sites: Vec::new(),
            known_publishers: Vec::new(),
            user_agent: None,
        }
    }
}

/// Search query configuration
#[derive(Debug, Clone)]
pub struct ArtworkSearchQuery {
    /// The game title to search for (primary)
    pub title: String,
    /// Alternative title (for OR search)
    pub alt_title: Option<String>,
    /// Optional region for more specific results
    pub region: Option<String>,
    /// Optional platform hint (e.g., "PlayStation", "Sega Saturn", "DOS", "PC")
    pub platform: Option<String>,
    /// Optional year
    pub year: Option<u32>,
    /// Search terms to append (e.g., "jewel case", "cover art")
    pub search_suffix: String,
    /// Publisher name (kept separate from title search)
    pub publisher: Option<String>,
    /// Original filename for context
    pub original_filename: Option<String>,
    /// Whether this is a Mac game (detected via HFS/HFS+ filesystem)
    pub is_mac_game: bool,
    /// Content type for query formatting
    pub content_type: ContentType,
}

impl ArtworkSearchQuery {
    /// Build default search suffix based on configuration
    fn build_default_suffix(config: &SearchConfig, original_filename: Option<&str>) -> String {
        let mut parts = Vec::new();

        // Determine if we should use CD or DVD keywords
        let is_dvd = original_filename
            .map(|f| f.to_lowercase().contains("dvd"))
            .unwrap_or(false);

        if is_dvd {
            // Use DVD keywords
            if !config.dvd_keywords.is_empty() {
                let dvd_terms: Vec<String> = config
                    .dvd_keywords
                    .iter()
                    .map(|k| format!("\"{}\"", k))
                    .collect();
                parts.push(dvd_terms.join(" OR "));
            }
        } else {
            // Use CD keywords
            if !config.cd_keywords.is_empty() {
                let cd_terms: Vec<String> = config
                    .cd_keywords
                    .iter()
                    .map(|k| format!("\"{}\"", k))
                    .collect();
                parts.push(cd_terms.join(" OR "));
            }
        }

        parts.push("art".to_string());

        // Add site inclusions or exclusions based on content type
        match config.content_type {
            ContentType::Games | ContentType::AppsUtilities | ContentType::AudioCDs => {
                // Limit to known sites for specific content types
                let sites = match config.content_type {
                    ContentType::Games => &config.games_sites,
                    ContentType::AppsUtilities => &config.apps_sites,
                    ContentType::AudioCDs => &config.audio_sites,
                    ContentType::Any => unreachable!(),
                };

                if !sites.is_empty() {
                    let site_terms: Vec<String> = sites
                        .iter()
                        .map(|s| format!("site:{}", s))
                        .collect();
                    parts.push(site_terms.join(" OR "));
                }
            }
            ContentType::Any => {
                // Don't limit sites, just add exclusions
                for site in &config.exclusion_sites {
                    parts.push(format!("-site:{}", site));
                }
            }
        }

        // Add platform exclusions (only for games and any)
        if config.content_type == ContentType::Games || config.content_type == ContentType::Any {
            for platform in &config.exclusion_platforms {
                parts.push(format!("-\"{}\"", platform));
            }
        }

        parts.join(" ")
    }

    /// Split CamelCase or PascalCase strings into separate words
    /// Example: "FinalFantasyVII" -> "Final Fantasy VII"
    fn split_camel_case(text: &str) -> String {
        // First, insert spaces between lowercase and uppercase letters
        let result = CAMEL_CASE_PATTERN.replace_all(text, |caps: &regex::Captures| {
            if let (Some(lower), Some(upper)) = (caps.get(1), caps.get(2)) {
                // Case: lowercase followed by uppercase (e.g., "aB")
                format!("{} {}", lower.as_str(), upper.as_str())
            } else if let (Some(uppers), Some(lower_part)) = (caps.get(3), caps.get(4)) {
                // Case: multiple uppercase followed by uppercase+lowercase (e.g., "ABCDe")
                format!("{} {}", uppers.as_str(), lower_part.as_str())
            } else {
                caps.get(0).unwrap().as_str().to_string()
            }
        });

        result.to_string()
    }

    /// Remove platform keywords from title if they appear with other descriptors
    /// Only removes if there are other meaningful words in the title
    fn remove_platform_keywords(title: &str) -> String {
        let words: Vec<&str> = title.split_whitespace().collect();
        
        // Don't modify if title is too short
        if words.len() <= 2 {
            return title.to_string();
        }

        let platform_keywords = [
            "Mac", "Macintosh", "Win", "Windows", "DOS", "PC",
        ];

        let filtered: Vec<&str> = words
            .into_iter()
            .filter(|&word| {
                // Keep the word if it's not a platform keyword
                !platform_keywords.iter().any(|&pk| {
                    word.eq_ignore_ascii_case(pk)
                })
            })
            .collect();

        // Only return filtered version if we still have meaningful content
        if filtered.len() >= 2 {
            filtered.join(" ")
        } else {
            title.to_string()
        }
    }

    /// Remove known publisher names from the title
    fn remove_publishers(title: &str, config: &SearchConfig) -> (String, Option<String>) {
        let mut detected_publisher = None;
        let mut cleaned_title = title.to_string();

        // Check for each known publisher
        for publisher in &config.known_publishers {
            let publisher_lower = publisher.to_lowercase();
            let title_lower = cleaned_title.to_lowercase();

            // Check if publisher is in the title (as a whole word)
            if let Some(pos) = title_lower.find(&publisher_lower) {
                // Verify it's a word boundary match
                let is_start = pos == 0 || !title_lower.chars().nth(pos - 1).unwrap().is_alphanumeric();
                let end_pos = pos + publisher_lower.len();
                let is_end = end_pos >= title_lower.len() || !title_lower.chars().nth(end_pos).unwrap().is_alphanumeric();

                if is_start && is_end {
                    // Found the publisher - remove it from title
                    detected_publisher = Some(publisher.clone());
                    
                    // Remove the publisher from the title
                    let before = &cleaned_title[..pos];
                    let after = if end_pos < cleaned_title.len() {
                        &cleaned_title[end_pos..]
                    } else {
                        ""
                    };
                    
                    cleaned_title = format!("{}{}", before, after)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    
                    break;
                }
            }
        }

        (cleaned_title, detected_publisher)
    }

    /// Process a title to handle CamelCase, platform keywords, and publishers
    fn process_title(title: &str, config: &SearchConfig) -> (String, Option<String>) {
        // First, try to split CamelCase
        let split_title = Self::split_camel_case(title);
        
        // Remove platform keywords if present with other descriptors
        let no_platform = Self::remove_platform_keywords(&split_title);
        
        // Remove known publishers and extract them
        let (cleaned_title, publisher) = Self::remove_publishers(&no_platform, config);
        
        (cleaned_title, publisher)
    }

    /// Create a search query from parsed filename information
    pub fn from_parsed_filename(parsed: &ParsedFilename) -> Self {
        Self::from_parsed_filename_with_config(parsed, &SearchConfig::default())
    }

    /// Create a search query from parsed filename information with custom config
    pub fn from_parsed_filename_with_config(parsed: &ParsedFilename, config: &SearchConfig) -> Self {
        let (title, publisher) = Self::process_title(&parsed.title, config);

        Self {
            title,
            alt_title: None,
            region: parsed.region.clone(),
            platform: None,
            year: parsed.year,
            search_suffix: Self::build_default_suffix(config, Some(&parsed.original)),
            publisher,
            original_filename: Some(parsed.original.clone()),
            is_mac_game: false, // Can't detect from filename alone
            content_type: config.content_type,
        }
    }

    /// Create a search query from disc info (uses best available title)
    pub fn from_disc_info(info: &DiscInfo) -> Self {
        Self::from_disc_info_with_config(info, &SearchConfig::default())
    }

    /// Create a search query from disc info with custom config
    pub fn from_disc_info_with_config(info: &DiscInfo, config: &SearchConfig) -> Self {
        let filename_title = info.parsed_filename.title.clone();

        // Detect if this is a Mac game based on:
        // 1. Filesystem type (HFS or HFS+)
        // 2. Folder path containing "mac"
        let is_mac_game = matches!(
            info.filesystem,
            crate::disc::FilesystemType::Hfs | crate::disc::FilesystemType::HfsPlus
        ) || is_mac_path(&info.path);

        // Get normalized volume label if available and useful
        let volume_title = info.volume_label.as_ref().and_then(|label| {
            // Volume labels are often short codes, so only use if it looks like a real title
            if label.len() > 4 && !label.chars().all(|c| c.is_uppercase() || c == '_') {
                Some(crate::disc::normalize_volume_label(label))
            } else {
                None
            }
        });

        // Process the filename title
        let (processed_filename, publisher_from_filename) = Self::process_title(&filename_title, config);

        // Process the volume title if available
        let processed_volume = volume_title.as_ref().map(|vol| {
            let (processed, _) = Self::process_title(vol, config);
            processed
        });

        // For Mac games, prefer volume label over filename (if available)
        // For other games, use filename as primary, volume label as alt
        let (title, alt_title) = match (is_mac_game, &processed_volume) {
            // Mac game with volume label: use volume label as primary
            (true, Some(vol)) if vol.to_lowercase() != processed_filename.to_lowercase() => {
                (vol.clone(), Some(processed_filename.clone()))
            }
            // Non-Mac or same title: use filename as primary, volume as alt if different
            (_, Some(vol)) if vol.to_lowercase() != processed_filename.to_lowercase() => {
                (processed_filename.clone(), Some(vol.clone()))
            }
            // Same title or no volume: just use one
            (_, Some(vol)) => (vol.clone(), None),
            (_, None) => (processed_filename, None),
        };

        // Detect platform from file path
        let platform = detect_platform_from_path(&info.path);

        Self {
            title,
            alt_title,
            region: info.parsed_filename.region.clone(),
            platform,
            year: info.parsed_filename.year,
            search_suffix: Self::build_default_suffix(config, Some(&info.parsed_filename.original)),
            publisher: publisher_from_filename,
            original_filename: Some(info.parsed_filename.original.clone()),
            is_mac_game,
            content_type: config.content_type,
        }
    }

    /// Set the platform hint
    pub fn with_platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = Some(platform.into());
        self
    }

    /// Set the publisher
    pub fn with_publisher(mut self, publisher: impl Into<String>) -> Self {
        self.publisher = Some(publisher.into());
        self
    }

    /// Set custom search suffix
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.search_suffix = suffix.into();
        self
    }

    /// Build the full search query string
    pub fn build_query(&self) -> String {
        // For Games content type, use a simpler query format
        if self.content_type == ContentType::Games {
            let platform = if self.is_mac_game { "mac" } else { "pc" };
            return format!("\"{}\" case {} site:mobygames.com", self.title, platform);
        }

        // For other content types, use the full query format
        let mut parts = Vec::new();

        // Build title part with OR if we have an alt title
        if let Some(ref alt) = self.alt_title {
            parts.push(format!("\"{}\" OR \"{}\"", self.title, alt));
        } else {
            parts.push(format!("\"{}\"", self.title));
        }

        // Add publisher if available (kept in search but separate from title)
        if let Some(ref publisher) = self.publisher {
            parts.push(format!("\"{}\"", publisher));
        }

        // Add year if available
        if let Some(year) = self.year {
            parts.push(year.to_string());
        }

        // Add platform if available
        if let Some(ref platform) = self.platform {
            parts.push(platform.clone());
        }

        if let Some(ref region) = self.region {
            // Convert region codes to more searchable terms
            let region_term = match region.to_uppercase().as_str() {
                "USA" | "NTSC-U" => "USA",
                "EUROPE" | "PAL" | "PAL-E" => "Europe",
                "JAPAN" | "NTSC-J" => "Japan",
                _ => region.as_str(),
            };
            parts.push(region_term.to_string());
        }

        parts.push(self.search_suffix.clone());

        parts.join(" ")
    }
}

/// Detect platform from file path
///
/// Looks for platform indicators in the directory path.
fn detect_platform_from_path(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy().to_lowercase();

    // Check for platform indicators in path (case-insensitive)
    if path_str.contains("/dos/") || path_str.contains("\\dos\\") || path_str.contains("/dos games/") {
        Some("DOS".to_string())
    } else if path_str.contains("/pc/") || path_str.contains("\\pc\\") || path_str.contains("/pc games/") {
        Some("PC".to_string())
    } else if path_str.contains("/macintosh/") || path_str.contains("\\macintosh\\") {
        Some("Macintosh".to_string())
    } else if path_str.contains("/mac/") || path_str.contains("\\mac\\") || path_str.contains("/mac games/") {
        Some("Mac".to_string())
    } else {
        None
    }
}

/// Check if file path indicates a Mac game
///
/// Returns true if any folder in the path contains "mac" (case insensitive).
fn is_mac_path(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().contains("mac")
}

impl ArtworkSearchQuery {
    /// Generate a Google Image search URL
    /// Uses tbs=iar:s to filter for square aspect ratio images
    pub fn google_images_url(&self) -> String {
        let query = self.build_query();
        let encoded = urlencoding::encode(&query);
        // tbs=iar:s filters for square aspect ratio images
        format!("https://www.google.com/search?tbm=isch&tbs=iar:s&q={}", encoded)
    }

    /// Generate a DuckDuckGo Image search URL
    /// Uses size:Square to filter for square images
    pub fn duckduckgo_images_url(&self) -> String {
        let query = self.build_query();
        let encoded = urlencoding::encode(&query);
        // size:Square filter for square images
        format!("https://duckduckgo.com/?t=h_&iax=images&ia=images&iaf=size:Square&q={}", encoded)
    }
}

/// Open a URL in the default system browser
pub fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| format!("Failed to open browser: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_build_query_simple() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            alt_title: None,
            region: None,
            platform: None,
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" CD art");
    }

    #[test]
    fn test_build_query_with_region() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            alt_title: None,
            region: Some("USA".to_string()),
            platform: None,
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" USA CD art");
    }

    #[test]
    fn test_build_query_with_platform() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            alt_title: None,
            region: Some("USA".to_string()),
            platform: Some("PlayStation".to_string()),
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        assert_eq!(
            query.build_query(),
            "\"Final Fantasy VII\" PlayStation USA CD art"
        );
    }

    #[test]
    fn test_build_query_with_or() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            alt_title: Some("FF7".to_string()),
            region: None,
            platform: None,
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" OR \"FF7\" CD art");
    }

    #[test]
    fn test_build_query_with_year() {
        let query = ArtworkSearchQuery {
            title: "Doom".to_string(),
            alt_title: None,
            region: None,
            platform: Some("DOS".to_string()),
            year: Some(1993),
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        assert_eq!(query.build_query(), "\"Doom\" 1993 DOS CD art");
    }

    #[test]
    fn test_build_query_with_publisher() {
        let query = ArtworkSearchQuery {
            title: "Command & Conquer".to_string(),
            alt_title: None,
            region: None,
            platform: None,
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: Some("Westwood".to_string()),
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        assert_eq!(query.build_query(), "\"Command & Conquer\" \"Westwood\" CD art");
    }

    #[test]
    fn test_build_query_games_pc() {
        let query = ArtworkSearchQuery {
            title: "Doom".to_string(),
            alt_title: None,
            region: None,
            platform: None,
            year: None,
            search_suffix: String::new(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Games,
        };

        assert_eq!(query.build_query(), "\"Doom\" case pc site:mobygames.com");
    }

    #[test]
    fn test_build_query_games_mac() {
        let query = ArtworkSearchQuery {
            title: "Myst".to_string(),
            alt_title: None,
            region: None,
            platform: None,
            year: None,
            search_suffix: String::new(),
            publisher: None,
            original_filename: None,
            is_mac_game: true,
            content_type: ContentType::Games,
        };

        assert_eq!(query.build_query(), "\"Myst\" case mac site:mobygames.com");
    }

    #[test]
    fn test_from_parsed_filename() {
        let parsed = crate::disc::parse_filename(Path::new("Final Fantasy VII (USA).iso"));
        let query = ArtworkSearchQuery::from_parsed_filename(&parsed);

        assert_eq!(query.title, "Final Fantasy VII");
        assert_eq!(query.region, Some("USA".to_string()));
        assert!(query.search_suffix.contains("art"));
    }

    #[test]
    fn test_split_camel_case() {
        assert_eq!(
            ArtworkSearchQuery::split_camel_case("FinalFantasyVII"),
            "Final Fantasy VII"
        );
        assert_eq!(
            ArtworkSearchQuery::split_camel_case("CommandAndConquer"),
            "Command And Conquer"
        );
        assert_eq!(
            ArtworkSearchQuery::split_camel_case("WarCraft"),
            "War Craft"
        );
        // Already has spaces - should not be affected
        assert_eq!(
            ArtworkSearchQuery::split_camel_case("Final Fantasy VII"),
            "Final Fantasy VII"
        );
    }

    #[test]
    fn test_remove_platform_keywords() {
        // Should remove platform keywords when there are other descriptors
        assert_eq!(
            ArtworkSearchQuery::remove_platform_keywords("Doom DOS Edition"),
            "Doom Edition"
        );
        assert_eq!(
            ArtworkSearchQuery::remove_platform_keywords("SimCity Mac Version"),
            "SimCity Version"
        );
        assert_eq!(
            ArtworkSearchQuery::remove_platform_keywords("Windows Solitaire Game"),
            "Solitaire Game"
        );

        // Should not modify short titles
        assert_eq!(
            ArtworkSearchQuery::remove_platform_keywords("Doom DOS"),
            "Doom DOS"
        );

        // Should not affect titles without platform keywords
        assert_eq!(
            ArtworkSearchQuery::remove_platform_keywords("Final Fantasy VII"),
            "Final Fantasy VII"
        );
    }

    #[test]
    fn test_remove_publishers() {
        let config = SearchConfig::default();

        let (title, publisher) = ArtworkSearchQuery::remove_publishers("Command & Conquer Westwood", &config);
        assert_eq!(title, "Command & Conquer");
        assert_eq!(publisher, Some("Westwood".to_string()));

        let (title, publisher) = ArtworkSearchQuery::remove_publishers("Sierra King's Quest", &config);
        assert_eq!(title, "King's Quest");
        assert_eq!(publisher, Some("Sierra".to_string()));

        let (title, publisher) = ArtworkSearchQuery::remove_publishers("Final Fantasy VII", &config);
        assert_eq!(title, "Final Fantasy VII");
        assert_eq!(publisher, None);

        // Test EA specifically
        let (title, publisher) = ArtworkSearchQuery::remove_publishers("Command & Conquer EA", &config);
        assert_eq!(title, "Command & Conquer");
        assert_eq!(publisher, Some("EA".to_string()));
    }

    #[test]
    fn test_process_title() {
        let config = SearchConfig::default();

        // Test CamelCase splitting
        let (title, _) = ArtworkSearchQuery::process_title("FinalFantasyVII", &config);
        assert_eq!(title, "Final Fantasy VII");

        // Test platform removal with CamelCase
        let (title, _) = ArtworkSearchQuery::process_title("DoomDOSEdition", &config);
        assert_eq!(title, "Doom Edition");

        // Test publisher detection
        let (title, publisher) = ArtworkSearchQuery::process_title("Command Conquer Westwood", &config);
        assert_eq!(title, "Command Conquer");
        assert_eq!(publisher, Some("Westwood".to_string()));
    }

    #[test]
    fn test_build_default_suffix_cd() {
        let mut config = SearchConfig::default();
        config.content_type = ContentType::Any; // Use Any to test without site limits
        let suffix = ArtworkSearchQuery::build_default_suffix(&config, Some("game.iso"));

        assert!(suffix.contains("art"));
        assert!(suffix.contains("-site:ebay.com"));
        assert!(suffix.contains("-\"playstation\""));
        // Should use CD keywords by default
        assert!(suffix.contains("CD") || suffix.contains("jewel case"));
    }

    #[test]
    fn test_build_default_suffix_dvd() {
        let mut config = SearchConfig::default();
        config.content_type = ContentType::Any; // Use Any to test without site limits
        let suffix = ArtworkSearchQuery::build_default_suffix(&config, Some("game_dvd.iso"));

        assert!(suffix.contains("art"));
        // Should use DVD keywords when DVD is in filename
        assert!(suffix.contains("DVD"));
    }

    #[test]
    fn test_detect_platform_dos() {
        let path = Path::new("/games/DOS/Doom.iso");
        assert_eq!(detect_platform_from_path(path), Some("DOS".to_string()));
    }

    #[test]
    fn test_detect_platform_pc() {
        let path = Path::new("/games/PC/Half-Life.iso");
        assert_eq!(detect_platform_from_path(path), Some("PC".to_string()));
    }

    #[test]
    fn test_detect_platform_mac() {
        let path = Path::new("/games/Macintosh/Marathon.iso");
        assert_eq!(detect_platform_from_path(path), Some("Macintosh".to_string()));
    }

    #[test]
    fn test_is_mac_path() {
        assert!(is_mac_path(Path::new("/games/Mac/Myst.iso")));
        assert!(is_mac_path(Path::new("/games/Macintosh/Marathon.iso")));
        assert!(is_mac_path(Path::new("C:\\Games\\MAC\\Myst.iso")));
        assert!(!is_mac_path(Path::new("/games/PC/Doom.iso")));
        assert!(!is_mac_path(Path::new("/games/DOS/Doom.iso")));
    }

    #[test]
    fn test_year_extraction() {
        let parsed = crate::disc::parse_filename(Path::new("Doom (1993).iso"));
        assert_eq!(parsed.year, Some(1993));
    }

    #[test]
    fn test_google_url() {
        let query = ArtworkSearchQuery {
            title: "Sonic Adventure".to_string(),
            alt_title: None,
            region: None,
            platform: Some("Dreamcast".to_string()),
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        let url = query.google_images_url();
        // Check for square filter and image search params
        assert!(url.starts_with("https://www.google.com/search?tbm=isch&tbs=iar:s&q="));
        assert!(url.contains("Sonic"));
    }

    #[test]
    fn test_duckduckgo_url_has_square_filter() {
        let query = ArtworkSearchQuery {
            title: "Test Game".to_string(),
            alt_title: None,
            region: None,
            platform: None,
            year: None,
            search_suffix: "CD art".to_string(),
            publisher: None,
            original_filename: None,
            is_mac_game: false,
            content_type: ContentType::Any,
        };

        let url = query.duckduckgo_images_url();
        assert!(url.contains("size:Square"));
    }

    #[test]
    fn test_content_type_conversions() {
        assert_eq!(ContentType::Games.as_str(), "games");
        assert_eq!(ContentType::AppsUtilities.as_str(), "apps_utilities");
        assert_eq!(ContentType::AudioCDs.as_str(), "audio_cds");
        assert_eq!(ContentType::Any.as_str(), "any");

        assert_eq!(ContentType::from_str("games"), ContentType::Games);
        assert_eq!(ContentType::from_str("apps"), ContentType::AppsUtilities);
        assert_eq!(ContentType::from_str("audio_cds"), ContentType::AudioCDs);
        assert_eq!(ContentType::from_str("any"), ContentType::Any);
        assert_eq!(ContentType::from_str("invalid"), ContentType::Any); // default

        assert_eq!(ContentType::Games.display_name(), "Games");
        assert_eq!(ContentType::AppsUtilities.display_name(), "Apps & Utilities");
        assert_eq!(ContentType::AudioCDs.display_name(), "Audio CDs");
        assert_eq!(ContentType::Any.display_name(), "Any");
    }

    #[test]
    fn test_build_suffix_with_site_limits_games() {
        let mut config = SearchConfig::default();
        config.content_type = ContentType::Games;
        config.games_sites = vec!["mobygames.com".to_string(), "archive.org".to_string()];

        let suffix = ArtworkSearchQuery::build_default_suffix(&config, Some("game.iso"));

        assert!(suffix.contains("art"));
        assert!(suffix.contains("site:mobygames.com"));
        assert!(suffix.contains("site:archive.org"));
        // Should NOT have exclusions when limiting to known sites
        assert!(!suffix.contains("-site:ebay.com"));
        // Should still have platform exclusions for games
        assert!(suffix.contains("-\"playstation\""));
    }

    #[test]
    fn test_build_suffix_with_site_limits_audio() {
        let mut config = SearchConfig::default();
        config.content_type = ContentType::AudioCDs;
        config.audio_sites = vec!["discogs.com".to_string(), "allmusic.com".to_string()];

        let suffix = ArtworkSearchQuery::build_default_suffix(&config, Some("album.iso"));

        assert!(suffix.contains("art"));
        assert!(suffix.contains("site:discogs.com"));
        assert!(suffix.contains("site:allmusic.com"));
        // Should NOT have platform exclusions for audio CDs
        assert!(!suffix.contains("-\"playstation\""));
    }

    #[test]
    fn test_build_suffix_any_type() {
        let mut config = SearchConfig::default();
        config.content_type = ContentType::Any;

        let suffix = ArtworkSearchQuery::build_default_suffix(&config, Some("game.iso"));

        assert!(suffix.contains("art"));
        // Should have exclusions for Any type
        assert!(suffix.contains("-site:ebay.com"));
        // Should NOT have site inclusions
        assert!(!suffix.contains("site:mobygames.com"));
        // Should have platform exclusions for Any
        assert!(suffix.contains("-\"playstation\""));
    }
}
