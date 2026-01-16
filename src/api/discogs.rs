//! Discogs API integration for album artwork
//!
//! Provides direct album search and cover art retrieval from Discogs.

use serde::Deserialize;
use crate::config::get_secrets;
use crate::crypto::get_embedded_secrets;

/// Discogs search response
#[derive(Debug, Deserialize)]
struct DiscogsSearchResponse {
    results: Vec<DiscogsSearchResult>,
}

/// A single search result from Discogs
#[derive(Debug, Deserialize)]
struct DiscogsSearchResult {
    id: u64,
    #[serde(rename = "type")]
    result_type: String,
    title: String,
    thumb: Option<String>,
    cover_image: Option<String>,
    resource_url: String,
}

/// Discogs release details
#[derive(Debug, Deserialize)]
struct DiscogsRelease {
    id: u64,
    title: String,
    artists: Option<Vec<DiscogsArtist>>,
    images: Option<Vec<DiscogsImage>>,
    year: Option<u32>,
}

/// Discogs master release details
#[derive(Debug, Deserialize)]
struct DiscogsMaster {
    id: u64,
    title: String,
    artists: Option<Vec<DiscogsArtist>>,
    images: Option<Vec<DiscogsImage>>,
    year: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DiscogsArtist {
    name: String,
}

#[derive(Debug, Deserialize)]
struct DiscogsImage {
    #[serde(rename = "type")]
    image_type: String,
    uri: String,
    uri150: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

/// Result from Discogs search
#[derive(Debug, Clone)]
pub struct DiscogsResult {
    pub title: String,
    pub artist: String,
    pub year: Option<u32>,
    pub image_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub discogs_id: u64,
    pub result_type: String,
}

fn build_client() -> Result<reqwest::blocking::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();

    // Try to get credentials in order of preference:
    // 1. Embedded secrets (release builds)
    // 2. secrets.json (local development)
    let (consumer_key, consumer_secret) = if let Some(embedded) = get_embedded_secrets() {
        if embedded.has_credentials() {
            log::debug!("Using embedded Discogs API credentials");
            (
                embedded.discogs_consumer_key.clone(),
                embedded.discogs_consumer_secret.clone(),
            )
        } else {
            (String::new(), String::new())
        }
    } else {
        // Fall back to secrets.json
        let secrets = get_secrets();
        if secrets.discogs.has_credentials() {
            log::debug!("Using Discogs API credentials from secrets.json");
            (
                secrets.discogs.consumer_key.clone(),
                secrets.discogs.consumer_secret.clone(),
            )
        } else {
            (String::new(), String::new())
        }
    };

    // Add authorization header if we have credentials
    if !consumer_key.is_empty() && !consumer_secret.is_empty() {
        let auth_value = format!("Discogs key={}, secret={}", consumer_key, consumer_secret);
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&auth_value)
                .map_err(|e| format!("Invalid auth header: {}", e))?,
        );
    } else {
        log::debug!("No Discogs API credentials available, using anonymous access");
    }

    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .default_headers(headers)
        .user_agent(concat!(
            "ODE-Artwork-Downloader/",
            env!("CARGO_PKG_VERSION"),
            " +https://github.com/danifunker/ODE-artwork-downloader"
        ))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))
}

/// Search Discogs for releases matching artist and title
pub fn search_release(artist: &str, title: &str) -> Result<Vec<DiscogsResult>, String> {
    let client = build_client()?;

    // Build search query - skip "Various Artists" type names
    let artist_lower = artist.to_lowercase();
    let query = if artist_lower == "various artists"
        || artist_lower == "various"
        || artist_lower == "va"
        || artist_lower == "unknown artist"
    {
        title.to_string()
    } else {
        format!("{} {}", artist, title)
    };

    log::info!("Discogs API search: artist='{}' title='{}'", artist, title);
    log::info!("Discogs API query: {}", query);

    let url = format!(
        "https://api.discogs.com/database/search?q={}&type=release&per_page=10",
        urlencoding::encode(&query)
    );

    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("Discogs API request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        log::error!("Discogs API error: {} - {}", status, body);
        return Err(format!("Discogs API error: {}", status));
    }

    let search_response: DiscogsSearchResponse = response
        .json()
        .map_err(|e| format!("Failed to parse Discogs response: {}", e))?;

    log::info!("Discogs API returned {} results", search_response.results.len());

    let mut results = Vec::new();

    for (i, item) in search_response.results.iter().enumerate() {
        log::debug!("  Discogs result {}: {} ({})", i + 1, item.title, item.result_type);

        // Parse artist and title from the combined title (usually "Artist - Title")
        let (parsed_artist, parsed_title) = if let Some((a, t)) = item.title.split_once(" - ") {
            (a.to_string(), t.to_string())
        } else {
            ("Unknown".to_string(), item.title.clone())
        };

        results.push(DiscogsResult {
            title: parsed_title,
            artist: parsed_artist,
            year: None,
            image_url: item.cover_image.clone(),
            thumbnail_url: item.thumb.clone(),
            discogs_id: item.id,
            result_type: item.result_type.clone(),
        });
    }

    Ok(results)
}

/// Get high-resolution cover art for a specific release
pub fn get_release_images(release_id: u64, is_master: bool) -> Result<Vec<String>, String> {
    let client = build_client()?;

    let url = if is_master {
        format!("https://api.discogs.com/masters/{}", release_id)
    } else {
        format!("https://api.discogs.com/releases/{}", release_id)
    };

    log::debug!("Fetching Discogs release details: {}", url);

    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("Failed to fetch release details: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("Failed to fetch release: {}", response.status()));
    }

    // Parse as generic JSON to handle both release and master
    let json: serde_json::Value = response
        .json()
        .map_err(|e| format!("Failed to parse release: {}", e))?;

    let mut image_urls = Vec::new();

    if let Some(images) = json["images"].as_array() {
        for img in images {
            // Prefer "primary" type images first
            if img["type"].as_str() == Some("primary") {
                if let Some(uri) = img["uri"].as_str() {
                    image_urls.insert(0, uri.to_string());
                }
            } else if let Some(uri) = img["uri"].as_str() {
                image_urls.push(uri.to_string());
            }
        }
    }

    Ok(image_urls)
}
