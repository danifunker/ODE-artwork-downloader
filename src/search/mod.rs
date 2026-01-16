//! Image search functionality
//!
//! Fetches image search results from DuckDuckGo and parses them for display.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use std::time::Duration;

/// A single image search result
#[derive(Debug, Clone)]
pub struct ImageResult {
    /// URL of the full-size image
    pub image_url: String,
    /// URL of the thumbnail
    pub thumbnail_url: String,
    /// Title/alt text of the image
    pub title: String,
    /// Source website
    pub source: String,
    /// Image width (if known)
    pub width: Option<u32>,
    /// Image height (if known)
    pub height: Option<u32>,
}

/// DuckDuckGo image result from their JSON API
#[derive(Debug, Deserialize)]
struct DdgImageResult {
    image: String,
    thumbnail: String,
    title: String,
    source: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

/// DuckDuckGo images API response
#[derive(Debug, Deserialize)]
struct DdgImagesResponse {
    results: Vec<DdgImageResult>,
}

/// Search for images using DuckDuckGo
pub fn search_images(query: &str, max_results: usize) -> Result<Vec<ImageResult>, String> {
    log::info!("DDG Search Query: {}", query);

    let client = build_client()?;

    // Step 1: Get the vqd token from the search page
    let vqd = get_vqd_token(&client, query)?;

    // Step 2: Fetch image results using the token
    let results = fetch_image_results(&client, query, &vqd, max_results)?;

    log::info!("DDG Search returned {} results", results.len());
    for (i, result) in results.iter().take(5).enumerate() {
        log::debug!("  Result {}: {} ({})", i + 1, result.title, result.source);
    }

    Ok(results)
}

fn build_client() -> Result<Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
        ),
    );

    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))
}

fn get_vqd_token(client: &Client, query: &str) -> Result<String, String> {
    let url = format!(
        "https://duckduckgo.com/?q={}&ia=images&iax=images",
        urlencoding::encode(query)
    );

    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("Failed to fetch search page: {}", e))?;

    let text = response
        .text()
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // Extract vqd token from the page
    // Look for: vqd="..." or vqd='...' or vqd=...&
    let patterns = [
        r#"vqd=["']([^"']+)["']"#,
        r#"vqd=([^&"']+)"#,
        r#"vqd%3D([^&]+)"#,
    ];

    for pattern in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&text) {
                if let Some(m) = caps.get(1) {
                    let token = m.as_str().to_string();
                    if !token.is_empty() && token.len() > 10 {
                        log::debug!("Found vqd token: {}", &token[..20.min(token.len())]);
                        return Ok(token);
                    }
                }
            }
        }
    }

    // Try alternate method - look in script tags
    if let Ok(re) = regex::Regex::new(r#"vqd\s*[:=]\s*["']?([a-zA-Z0-9_-]+)"#) {
        if let Some(caps) = re.captures(&text) {
            if let Some(m) = caps.get(1) {
                let token = m.as_str().to_string();
                if token.len() > 10 {
                    return Ok(token);
                }
            }
        }
    }

    Err("Could not find vqd token in search page".to_string())
}

fn fetch_image_results(
    client: &Client,
    query: &str,
    vqd: &str,
    max_results: usize,
) -> Result<Vec<ImageResult>, String> {
    let url = format!(
        "https://duckduckgo.com/i.js?l=us-en&o=json&q={}&vqd={}&f=,,,,,&p=1",
        urlencoding::encode(query),
        urlencoding::encode(vqd)
    );

    log::debug!("Fetching images from: {}", url);

    let response = client
        .get(&url)
        .header("Accept", "application/json")
        .header("Referer", "https://duckduckgo.com/")
        .send()
        .map_err(|e| format!("Failed to fetch images: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("Image search failed with status: {}", status));
    }

    let text = response
        .text()
        .map_err(|e| format!("Failed to read image results: {}", e))?;

    log::debug!("Response length: {} bytes", text.len());

    // Parse JSON response
    let ddg_response: DdgImagesResponse = serde_json::from_str(&text)
        .map_err(|e| format!("Failed to parse image results: {} (response: {}...)", e, &text[..200.min(text.len())]))?;

    let results: Vec<ImageResult> = ddg_response
        .results
        .into_iter()
        .take(max_results)
        .map(|r| ImageResult {
            image_url: r.image,
            thumbnail_url: r.thumbnail,
            title: r.title,
            source: r.source.unwrap_or_default(),
            width: r.width,
            height: r.height,
        })
        .collect();

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires network access
    fn test_search_images() {
        let results = search_images("rust programming language logo", 5).unwrap();
        assert!(!results.is_empty());
        for result in &results {
            assert!(!result.image_url.is_empty());
            println!("Found: {} - {}", result.title, result.image_url);
        }
    }
}
