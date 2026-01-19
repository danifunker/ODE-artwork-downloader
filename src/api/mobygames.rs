//! MobyGames cover art scraper
//!
//! Uses headless Chromium to bypass Cloudflare protection and scrape
//! cover art from MobyGames game pages.

use headless_chrome::{Browser, LaunchOptions};
use std::time::Duration;

/// Cover information extracted from MobyGames
#[derive(Debug, Clone)]
pub struct CoverInfo {
    /// Platform name (e.g., "DOS", "Windows", "Macintosh")
    pub platform: String,
    /// Country/region (e.g., "United States", "United Kingdom")
    pub country: String,
    /// Type of cover (e.g., "Front Cover", "Back Cover")
    pub cover_type: String,
    /// Thumbnail image URL
    pub thumbnail_url: String,
    /// Full-size image URL
    pub full_url: String,
}

/// Platform priority for cover selection (most preferred first)
const PLATFORM_PRIORITY: &[&str] = &[
    "DOS",
    "Windows",
    "Windows 3.x",
    "Macintosh",
    "Linux",
    "PC-98",
    "Amiga",
];

/// Search for game covers on MobyGames
///
/// 1. Uses DuckDuckGo to find the MobyGames game page
/// 2. Uses headless Chromium to navigate to the covers page
/// 3. Parses the HTML to find platform-specific covers
/// 4. Returns the best match based on platform priority
pub fn search_game_covers(
    title: &str,
    user_agent: Option<&str>,
) -> Result<Vec<crate::search::ImageResult>, String> {
    log::info!("MobyGames search for: {}", title);

    // Step 1: DDG search to find MobyGames game page
    let query = format!("\"{}\" site:mobygames.com", title);
    let web_results = crate::search::ddg_web_search(&query, user_agent, 10)?;

    // Step 2: Find MobyGames game URL from results
    let game_url = web_results
        .iter()
        .find(|r| r.url.contains("mobygames.com/game/"))
        .map(|r| &r.url)
        .ok_or_else(|| format!("No MobyGames result found for '{}'", title))?;

    log::info!("Found MobyGames game page: {}", game_url);

    // Step 3: Launch headless Chromium and navigate to covers page
    let covers = fetch_covers_with_chromium(game_url)?;

    if covers.is_empty() {
        return Err("No covers found on MobyGames page".to_string());
    }

    log::info!("Found {} covers on MobyGames", covers.len());

    // Step 4: Find best cover based on platform priority
    let best_cover = find_best_cover(&covers)
        .ok_or_else(|| "No suitable cover found (no front covers)".to_string())?;

    log::info!(
        "Selected cover: {} - {} ({})",
        best_cover.platform,
        best_cover.country,
        best_cover.cover_type
    );

    // Step 5: Convert to ImageResult
    Ok(vec![crate::search::ImageResult {
        image_url: best_cover.full_url.clone(),
        thumbnail_url: best_cover.thumbnail_url.clone(),
        title: format!(
            "{} - {} ({})",
            title, best_cover.platform, best_cover.country
        ),
        source: "MobyGames".to_string(),
        width: None,
        height: None,
    }])
}

/// Fetch covers page using headless Chromium
fn fetch_covers_with_chromium(game_url: &str) -> Result<Vec<CoverInfo>, String> {
    log::info!("Launching headless Chromium...");

    // Configure browser launch options
    let launch_options = LaunchOptions::default_builder()
        .headless(true)
        .idle_browser_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("Failed to configure browser: {}", e))?;

    // Launch browser (will auto-download Chromium if needed)
    let browser = Browser::new(launch_options)
        .map_err(|e| format!("Failed to launch browser: {}", e))?;

    // Create new tab
    let tab = browser
        .new_tab()
        .map_err(|e| format!("Failed to create tab: {}", e))?;

    // Build covers page URL
    let covers_url = format!("{}/covers/", game_url.trim_end_matches('/'));
    log::info!("Navigating to covers page: {}", covers_url);

    // Navigate to covers page
    tab.navigate_to(&covers_url)
        .map_err(|e| format!("Failed to navigate: {}", e))?;

    // Wait for page to load
    tab.wait_until_navigated()
        .map_err(|e| format!("Navigation timeout: {}", e))?;

    // Give the page a moment to fully render
    std::thread::sleep(Duration::from_millis(1000));

    // Get page content
    let html = tab
        .get_content()
        .map_err(|e| format!("Failed to get page content: {}", e))?;

    log::debug!("Got {} bytes of HTML from covers page", html.len());

    // Parse covers from HTML
    parse_covers_html(&html)
}

/// Parse MobyGames covers page HTML to extract cover information
fn parse_covers_html(html: &str) -> Result<Vec<CoverInfo>, String> {
    let mut covers = Vec::new();

    // MobyGames covers page structure (based on typical structure):
    // Each cover is in a section with platform headers
    // Cover images are in thumbnail containers with links to full images

    // Pattern to find cover entries
    // Looking for image thumbnails with cover info
    let _cover_pattern = regex::Regex::new(
        r#"<a[^>]*href="(/game/[^"]+/cover/[^"]+)"[^>]*>.*?<img[^>]*src="([^"]+)"[^>]*/?>.*?</a>"#
    ).map_err(|e| format!("Failed to compile cover regex: {}", e))?;

    // Pattern to find platform sections
    let platform_pattern = regex::Regex::new(
        r#"<h2[^>]*>([^<]+)</h2>"#
    ).map_err(|e| format!("Failed to compile platform regex: {}", e))?;

    // Find all platform headers and their positions
    let mut platform_positions: Vec<(usize, String)> = platform_pattern
        .captures_iter(html)
        .filter_map(|caps| {
            let full_match = caps.get(0)?;
            let platform = caps.get(1)?.as_str().trim().to_string();
            Some((full_match.start(), platform))
        })
        .collect();

    // Sort by position
    platform_positions.sort_by_key(|(pos, _)| *pos);

    // Alternative: Try to parse cover-art-group structure
    // <div class="cover-art-group">
    let group_pattern = regex::Regex::new(
        r#"(?s)<div[^>]*class="[^"]*coverHeading[^"]*"[^>]*>([^<]+)</div>.*?<img[^>]*src="([^"]+)"#
    ).map_err(|e| format!("Failed to compile group regex: {}", e))?;

    for caps in group_pattern.captures_iter(html) {
        if let (Some(info_match), Some(img_match)) = (caps.get(1), caps.get(2)) {
            let info = info_match.as_str().trim();
            let thumbnail_url = img_match.as_str().to_string();

            // Parse info string (e.g., "DOS (United States)")
            let (platform, country) = parse_cover_info(info);

            // Convert thumbnail URL to full-size URL
            let full_url = thumbnail_to_full_url(&thumbnail_url);

            covers.push(CoverInfo {
                platform,
                country,
                cover_type: "Front Cover".to_string(),
                thumbnail_url,
                full_url,
            });
        }
    }

    // If group pattern didn't work, try simpler image-based extraction
    if covers.is_empty() {
        // Look for any cover images
        let img_pattern = regex::Regex::new(
            r#"<img[^>]*src="(https://cdn\.mobygames\.com/[^"]+)"[^>]*alt="([^"]*)"#
        ).map_err(|e| format!("Failed to compile img regex: {}", e))?;

        for caps in img_pattern.captures_iter(html) {
            if let (Some(src_match), Some(alt_match)) = (caps.get(1), caps.get(2)) {
                let thumbnail_url = src_match.as_str().to_string();
                let alt = alt_match.as_str();

                // Skip non-cover images
                if !alt.to_lowercase().contains("cover") && !thumbnail_url.contains("/covers/") {
                    continue;
                }

                let (platform, country) = parse_cover_info(alt);
                let full_url = thumbnail_to_full_url(&thumbnail_url);

                covers.push(CoverInfo {
                    platform,
                    country,
                    cover_type: if alt.to_lowercase().contains("back") {
                        "Back Cover".to_string()
                    } else {
                        "Front Cover".to_string()
                    },
                    thumbnail_url,
                    full_url,
                });
            }
        }
    }

    // Even simpler fallback: just find any MobyGames CDN images
    if covers.is_empty() {
        let cdn_pattern = regex::Regex::new(
            r#"https://cdn\.mobygames\.com/[a-f0-9\-]+/covers/[^"'\s]+"#
        ).map_err(|e| format!("Failed to compile cdn regex: {}", e))?;

        for mat in cdn_pattern.find_iter(html) {
            let url = mat.as_str().to_string();
            let full_url = thumbnail_to_full_url(&url);

            covers.push(CoverInfo {
                platform: "Unknown".to_string(),
                country: "Unknown".to_string(),
                cover_type: "Front Cover".to_string(),
                thumbnail_url: url,
                full_url,
            });
        }
    }

    Ok(covers)
}

/// Parse cover info string like "DOS (United States)" into (platform, country)
fn parse_cover_info(info: &str) -> (String, String) {
    // Try to extract platform and country from formats like:
    // "DOS (United States)"
    // "Windows - Front Cover"
    // "Macintosh / United Kingdom"

    let info = info.trim();

    // Try parentheses format: "Platform (Country)"
    if let Some(paren_start) = info.find('(') {
        if let Some(paren_end) = info.find(')') {
            let platform = info[..paren_start].trim().to_string();
            let country = info[paren_start + 1..paren_end].trim().to_string();
            return (platform, country);
        }
    }

    // Try dash format: "Platform - Type"
    if let Some(dash_pos) = info.find(" - ") {
        let platform = info[..dash_pos].trim().to_string();
        return (platform, "Unknown".to_string());
    }

    // Try slash format: "Platform / Country"
    if let Some(slash_pos) = info.find(" / ") {
        let platform = info[..slash_pos].trim().to_string();
        let country = info[slash_pos + 3..].trim().to_string();
        return (platform, country);
    }

    // Just use the whole string as platform
    (info.to_string(), "Unknown".to_string())
}

/// Convert thumbnail URL to full-size image URL
fn thumbnail_to_full_url(thumbnail_url: &str) -> String {
    // MobyGames CDN URLs often have size suffixes
    // Try to get the largest version by removing size constraints

    let url = thumbnail_url.to_string();

    // If it contains "small" or "medium", try to get "large"
    if url.contains("/small/") {
        return url.replace("/small/", "/large/");
    }
    if url.contains("/medium/") {
        return url.replace("/medium/", "/large/");
    }

    // If it has size in query params, remove them
    if let Some(query_start) = url.find('?') {
        return url[..query_start].to_string();
    }

    url
}

/// Find the best cover based on platform priority and language preference
fn find_best_cover(covers: &[CoverInfo]) -> Option<&CoverInfo> {
    // Priority 1: Try each platform in order, looking for US/UK front covers
    for platform in PLATFORM_PRIORITY {
        if let Some(cover) = covers
            .iter()
            .filter(|c| c.platform.to_lowercase().contains(&platform.to_lowercase()))
            .filter(|c| {
                c.country.contains("United States")
                    || c.country.contains("United Kingdom")
                    || c.country.contains("World")
                    || c.country == "Unknown"
            })
            .filter(|c| c.cover_type.to_lowercase().contains("front"))
            .next()
        {
            return Some(cover);
        }
    }

    // Priority 2: Any platform with US/UK front cover
    if let Some(cover) = covers
        .iter()
        .filter(|c| {
            c.country.contains("United States")
                || c.country.contains("United Kingdom")
                || c.country.contains("World")
        })
        .filter(|c| c.cover_type.to_lowercase().contains("front"))
        .next()
    {
        return Some(cover);
    }

    // Priority 3: Any front cover from priority platforms
    for platform in PLATFORM_PRIORITY {
        if let Some(cover) = covers
            .iter()
            .filter(|c| c.platform.to_lowercase().contains(&platform.to_lowercase()))
            .filter(|c| c.cover_type.to_lowercase().contains("front"))
            .next()
        {
            return Some(cover);
        }
    }

    // Priority 4: Any front cover
    covers
        .iter()
        .filter(|c| c.cover_type.to_lowercase().contains("front"))
        .next()

    // Note: We don't fall back to non-front covers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cover_info_parentheses() {
        let (platform, country) = parse_cover_info("DOS (United States)");
        assert_eq!(platform, "DOS");
        assert_eq!(country, "United States");
    }

    #[test]
    fn test_parse_cover_info_dash() {
        let (platform, country) = parse_cover_info("Windows - Front Cover");
        assert_eq!(platform, "Windows");
        assert_eq!(country, "Unknown");
    }

    #[test]
    fn test_parse_cover_info_slash() {
        let (platform, country) = parse_cover_info("Macintosh / United Kingdom");
        assert_eq!(platform, "Macintosh");
        assert_eq!(country, "United Kingdom");
    }

    #[test]
    fn test_thumbnail_to_full_url() {
        assert_eq!(
            thumbnail_to_full_url("https://cdn.mobygames.com/abc/covers/small/123.jpg"),
            "https://cdn.mobygames.com/abc/covers/large/123.jpg"
        );
    }
}
