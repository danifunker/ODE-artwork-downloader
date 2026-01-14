//! MusicBrainz API integration for audio CD identification
//!
//! Provides disc lookup and cover art retrieval from MusicBrainz

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CoverArtArchiveResponse {
    images: Vec<CoverArtImage>,
}

#[derive(Debug, Deserialize)]
struct CoverArtImage {
    image: String,
    thumbnails: Option<CoverArtThumbnails>,
    front: bool,
}

#[derive(Debug, Deserialize)]
struct CoverArtThumbnails {
    #[serde(rename = "250")]
    small: Option<String>,
    #[serde(rename = "500")]
    medium: Option<String>,
    #[serde(rename = "1200")]
    large: Option<String>,
}

/// MusicBrainz API result
#[derive(Debug, Clone)]
pub struct MusicBrainzResult {
    /// Album/release title
    pub title: String,
    /// Artist name
    pub artist: String,
    /// Release date (if available)
    pub date: Option<String>,
    /// MusicBrainz release ID
    pub release_id: String,
    /// Cover Art Archive URL (if available)
    pub cover_art_url: Option<String>,
    /// Thumbnail URL (if available)
    pub thumbnail_url: Option<String>,
}

/// Query MusicBrainz for releases matching the disc ID
/// If toc_string is provided, it will be used for fuzzy matching when disc ID lookup fails
pub fn search_by_discid(disc_id: &str, toc_string: Option<&str>) -> Result<Vec<MusicBrainzResult>, String> {
    log::info!("Querying MusicBrainz for disc ID: {}", disc_id);

    // Build the URL manually for disc ID lookup
    let mut url = format!(
        "https://musicbrainz.org/ws/2/discid/{}?fmt=json&inc=artist-credits+release-groups",
        disc_id
    );
    
    // Add TOC parameter for fuzzy matching if provided
    if let Some(toc) = toc_string {
        url.push_str(&format!("&toc={}", toc));
        log::info!("Added TOC for fuzzy matching: {}", toc);
    }

    // Use reqwest directly since musicbrainz_rs doesn't have disc ID lookup built-in
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!(
            "ODE-Artwork-Downloader/",
            env!("CARGO_PKG_VERSION"),
            " ( https://github.com/dani/ODE-artwork-downloader )"
        ))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("MusicBrainz lookup failed: {}", e))?;

    if !response.status().is_success() {
        if response.status().as_u16() == 404 {
            log::warn!("No releases found for disc ID: {}", disc_id);
            return Ok(Vec::new());
        }
        let status = response.status();
        let body = response.text().unwrap_or_default();
        log::error!("MusicBrainz API error response: {}", body);
        return Err(format!("MusicBrainz API error: {} - {}", status, body));
    }

    // Parse the JSON response manually
    let json: serde_json::Value = response
        .json()
        .map_err(|e| format!("Failed to parse MusicBrainz response: {}", e))?;

    let releases = json["releases"]
        .as_array()
        .ok_or("Invalid response format")?;

    if releases.is_empty() {
        log::warn!("No releases found for disc ID: {}", disc_id);
        return Ok(Vec::new());
    }

    log::info!("Found {} release(s) for disc ID", releases.len());

    // Convert to our result format and fetch cover art
    let mut results = Vec::new();
    for release in releases {
        let release_id = release["id"]
            .as_str()
            .ok_or("Missing release ID")?
            .to_string();

        let title = release["title"]
            .as_str()
            .unwrap_or("Unknown Album")
            .to_string();

        let artist = release["artist-credit"]
            .as_array()
            .and_then(|credits| credits.first())
            .and_then(|credit| credit["name"].as_str())
            .unwrap_or("Unknown Artist")
            .to_string();

        let date = release["date"].as_str().map(|s| s.to_string());

        // Try to get cover art
        let (cover_art_url, thumbnail_url) = match get_cover_art(&release_id) {
            Ok((cover, thumb)) => (Some(cover), thumb),
            Err(e) => {
                log::warn!("Failed to get cover art for {}: {}", release_id, e);
                (None, None)
            }
        };

        results.push(MusicBrainzResult {
            title,
            artist,
            date,
            release_id,
            cover_art_url,
            thumbnail_url,
        });
    }

    Ok(results)
}

/// Get cover art URL from Cover Art Archive
fn get_cover_art(release_id: &str) -> Result<(String, Option<String>), String> {
    let url = format!("https://coverartarchive.org/release/{}", release_id);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .user_agent(concat!(
            "ODE-Artwork-Downloader/",
            env!("CARGO_PKG_VERSION"),
            " ( https://github.com/dani/ODE-artwork-downloader )"
        ))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("Failed to query Cover Art Archive: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Cover Art Archive error: {}", response.status()));
    }

    let coverart: CoverArtArchiveResponse = response
        .json()
        .map_err(|e| format!("Failed to parse cover art response: {}", e))?;

    // Find front cover, or use first image
    let image = coverart
        .images
        .iter()
        .find(|img| img.front)
        .or_else(|| coverart.images.first())
        .ok_or("No cover art found")?;

    let thumbnail = image
        .thumbnails
        .as_ref()
        .and_then(|t| {
            t.large
                .clone()
                .or_else(|| t.medium.clone())
                .or_else(|| t.small.clone())
        });

    Ok((image.image.clone(), thumbnail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires network access
    fn test_search_by_discid() {
        // Example disc ID for testing (Pink Floyd - The Wall)
        let disc_id = "Wn8eRBtd9vAbzyhjiSRQ_ZQT49w-";
        let results = search_by_discid(disc_id, None).unwrap();
        assert!(!results.is_empty());
    }
}
