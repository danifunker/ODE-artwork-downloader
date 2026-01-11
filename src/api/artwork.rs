//! Artwork search functionality
//!
//! Provides cover art search using web image searches based on parsed filename information.

use crate::disc::{DiscInfo, ParsedFilename};

/// Search query configuration
#[derive(Debug, Clone)]
pub struct ArtworkSearchQuery {
    /// The game title to search for
    pub title: String,
    /// Optional region for more specific results
    pub region: Option<String>,
    /// Optional platform hint (e.g., "PlayStation", "Sega Saturn")
    pub platform: Option<String>,
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
            region: parsed.region.clone(),
            platform: None,
            search_suffix: Self::DEFAULT_SUFFIX.to_string(),
        }
    }

    /// Create a search query from disc info (uses best available title)
    pub fn from_disc_info(info: &DiscInfo) -> Self {
        // Prefer volume label if it looks like a real title, otherwise use parsed filename
        let title = if let Some(ref label) = info.volume_label {
            // Volume labels are often short codes, so use parsed title if label is too short
            if label.len() > 4 && !label.chars().all(|c| c.is_uppercase() || c == '_') {
                crate::disc::normalize_volume_label(label)
            } else {
                info.parsed_filename.title.clone()
            }
        } else {
            info.parsed_filename.title.clone()
        };

        Self {
            title,
            region: info.parsed_filename.region.clone(),
            platform: None,
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
        let mut parts = vec![format!("\"{}\"", self.title)];

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
            region: None,
            platform: None,
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" jewel case art");
    }

    #[test]
    fn test_build_query_with_region() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            region: Some("USA".to_string()),
            platform: None,
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(query.build_query(), "\"Final Fantasy VII\" USA jewel case art");
    }

    #[test]
    fn test_build_query_with_platform() {
        let query = ArtworkSearchQuery {
            title: "Final Fantasy VII".to_string(),
            region: Some("USA".to_string()),
            platform: Some("PlayStation".to_string()),
            search_suffix: "jewel case art".to_string(),
        };

        assert_eq!(
            query.build_query(),
            "\"Final Fantasy VII\" PlayStation USA jewel case art"
        );
    }

    #[test]
    fn test_from_parsed_filename() {
        let parsed = crate::disc::parse_filename(Path::new("Final Fantasy VII (USA).iso"));
        let query = ArtworkSearchQuery::from_parsed_filename(&parsed);

        assert_eq!(query.title, "Final Fantasy VII");
        assert_eq!(query.region, Some("USA".to_string()));
        assert!(query.search_suffix.contains("jewel case art"));
        assert!(query.search_suffix.contains("-site:ebay.com"));
    }

    #[test]
    fn test_google_url() {
        let query = ArtworkSearchQuery {
            title: "Sonic Adventure".to_string(),
            region: None,
            platform: Some("Dreamcast".to_string()),
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
            region: None,
            platform: None,
            search_suffix: "jewel case art".to_string(),
        };

        let url = query.duckduckgo_images_url();
        assert!(url.contains("size:Square"));
    }
}
