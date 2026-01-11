//! Artwork search functionality
//!
//! Provides cover art search using web image searches based on parsed filename information.

use crate::disc::{DiscInfo, ParsedFilename};
use std::path::Path;

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
}

impl ArtworkSearchQuery {
    /// Default search suffix for jewel case art searches
    const DEFAULT_SUFFIX: &'static str = "\"jewel case\" art -site:ebay.com -\"playstation\" -\"xbox\" -\"nintendo\"";

    /// Create a search query from parsed filename information
    pub fn from_parsed_filename(parsed: &ParsedFilename) -> Self {
        Self {
            title: parsed.title.clone(),
            alt_title: None,
            region: parsed.region.clone(),
            platform: None,
            year: parsed.year,
            search_suffix: Self::DEFAULT_SUFFIX.to_string(),
        }
    }

    /// Create a search query from disc info (uses best available title)
    pub fn from_disc_info(info: &DiscInfo) -> Self {
        let filename_title = info.parsed_filename.title.clone();

        // Get normalized volume label if available and useful
        let volume_title = info.volume_label.as_ref().and_then(|label| {
            // Volume labels are often short codes, so only use if it looks like a real title
            if label.len() > 4 && !label.chars().all(|c| c.is_uppercase() || c == '_') {
                Some(crate::disc::normalize_volume_label(label))
            } else {
                None
            }
        });

        // Use filename as primary, volume label as alt if different
        let (title, alt_title) = match volume_title {
            Some(ref vol) if vol.to_lowercase() != filename_title.to_lowercase() => {
                (filename_title.clone(), Some(vol.clone()))
            }
            Some(vol) => (vol, None), // Same title, just use one
            None => (filename_title, None),
        };

        // Detect platform from file path
        let platform = detect_platform_from_path(&info.path);

        Self {
            title,
            alt_title,
            region: info.parsed_filename.region.clone(),
            platform,
            year: info.parsed_filename.year,
            search_suffix: Self::DEFAULT_SUFFIX.to_string(),
        }
    }

    /// Set the platform hint
    pub fn with_platform(mut self, platform: impl Into<String>) -> Self {
        self.platform = Some(platform.into());
        self
    }

    /// Set custom search suffix
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.search_suffix = suffix.into();
        self
    }

    /// Build the full search query string
    pub fn build_query(&self) -> String {
        let mut parts = Vec::new();

        // Build title part with OR if we have an alt title
        if let Some(ref alt) = self.alt_title {
            parts.push(format!("(\"{}\" OR \"{}\")", self.title, alt));
        } else {
            parts.push(format!("\"{}\"", self.title));
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
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" jewel case art");
    }

    #[test]
    fn test_build_query_with_region() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            alt_title: None,
            region: Some("USA".to_string()),
            platform: None,
            year: None,
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" USA jewel case art");
    }

    #[test]
    fn test_build_query_with_platform() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            alt_title: None,
            region: Some("USA".to_string()),
            platform: Some("PlayStation".to_string()),
            year: None,
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(
            query.build_query(),
            "\"Final Fantasy VII\" PlayStation USA jewel case art"
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
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(query.build_query(), "(\"Final Fantasy VII\" OR \"FF7\") jewel case art");
    }

    #[test]
    fn test_build_query_with_year() {
        let query = ArtworkSearchQuery {
            title: "Doom".to_string(),
            alt_title: None,
            region: None,
            platform: Some("DOS".to_string()),
            year: Some(1993),
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(query.build_query(), "\"Doom\" 1993 DOS jewel case art");
    }

    #[test]
    fn test_from_parsed_filename() {
        let parsed = crate::disc::parse_filename(Path::new("Final Fantasy VII (USA).iso"));
        let query = ArtworkSearchQuery::from_parsed_filename(&parsed);

        assert_eq!(query.title, "Final Fantasy VII");
        assert_eq!(query.region, Some("USA".to_string()));
        assert!(query.search_suffix.contains("jewel case"));
        assert!(query.search_suffix.contains("-site:ebay.com"));
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
            search_suffix: "jewel case art".to_string(),
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
            search_suffix: "jewel case art".to_string(),
        };

        let url = query.duckduckgo_images_url();
        assert!(url.contains("size:Square"));
    }
}
