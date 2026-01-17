//! Image search functionality
//!
//! Fetches image search results from DuckDuckGo and parses them for display.

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use std::time::Duration;

/// Default user agent used when none is configured
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

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
    search_images_with_ua(query, max_results, None)
}

/// Search for images using DuckDuckGo with a custom user agent
pub fn search_images_with_ua(query: &str, max_results: usize, user_agent: Option<&str>) -> Result<Vec<ImageResult>, String> {
    log::info!("DDG Search Query: {}", query);

    let client = build_client(user_agent)?;

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

fn build_client(user_agent: Option<&str>) -> Result<Client, String> {
    let ua = user_agent.unwrap_or(DEFAULT_USER_AGENT);
    log::debug!("Using user agent: {}", ua);

    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(ua)
            .map_err(|e| format!("Invalid user agent string: {}", e))?,
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

/// Capture the user's browser user agent by starting a local HTTP server
/// and having the user navigate to it in their browser.
///
/// Returns the captured user agent string on success.
pub fn capture_browser_user_agent() -> Result<String, String> {
    use tiny_http::{Server, Response};

    // Start server on a random available port
    let server = Server::http("127.0.0.1:0")
        .map_err(|e| format!("Failed to start local server: {}", e))?;

    let addr = server.server_addr()
        .to_ip()
        .ok_or_else(|| "Failed to get server address".to_string())?;
    let url = format!("http://{}", addr);

    log::info!("User agent capture server started at {}", url);

    // Open URL in default browser
    open_url_in_browser(&url)?;

    // Wait for a request (with 60 second timeout)
    let request = server
        .recv_timeout(Duration::from_secs(60))
        .map_err(|e| format!("Error waiting for browser: {}", e))?
        .ok_or_else(|| "Timeout waiting for browser connection (60s)".to_string())?;

    // Extract user agent from request headers
    let user_agent = request
        .headers()
        .iter()
        .find(|h| h.field.as_str().to_ascii_lowercase() == "user-agent")
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    if user_agent.is_empty() {
        // Send error response
        let html = r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body style="font-family: sans-serif; text-align: center; padding: 50px;">
<h1>Error</h1>
<p>Could not detect browser user agent.</p>
<p>You can close this tab.</p>
</body></html>"#;
        let response = Response::from_string(html)
            .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap());
        let _ = request.respond(response);
        return Err("Browser did not send a user agent".to_string());
    }

    log::info!("Captured user agent: {}", user_agent);

    // Send success response
    let html = format!(r#"<!DOCTYPE html>
<html><head><title>Success</title></head>
<body style="font-family: sans-serif; text-align: center; padding: 50px;">
<h1 style="color: green;">Success!</h1>
<p>Browser identity captured successfully.</p>
<p style="font-size: 12px; color: #666; word-break: break-all; max-width: 600px; margin: 20px auto;">{}</p>
<p>You can close this tab.</p>
</body></html>"#, user_agent);

    let response = Response::from_string(html)
        .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap());
    let _ = request.respond(response);

    Ok(user_agent)
}

/// Open a URL in the system's default browser
fn open_url_in_browser(url: &str) -> Result<(), String> {
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
            .args(["/c", "start", "", url])
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

/// Save user agent to config.json
pub fn save_user_agent_to_config(user_agent: &str) -> Result<(), String> {
    let config_path = "config.json";

    // Read existing config
    let config_str = std::fs::read_to_string(config_path)
        .map_err(|e| format!("Failed to read config.json: {}", e))?;

    let mut json: serde_json::Value = serde_json::from_str(&config_str)
        .map_err(|e| format!("Failed to parse config.json: {}", e))?;

    // Update or create the search.user_agent field
    if let Some(search) = json.get_mut("search") {
        if let Some(obj) = search.as_object_mut() {
            obj.insert("user_agent".to_string(), serde_json::Value::String(user_agent.to_string()));
        }
    } else {
        // Create search section if it doesn't exist
        json["search"] = serde_json::json!({
            "user_agent": user_agent
        });
    }

    // Write back to file
    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(config_path, updated)
        .map_err(|e| format!("Failed to write config.json: {}", e))?;

    log::info!("Saved user agent to config.json");
    Ok(())
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
