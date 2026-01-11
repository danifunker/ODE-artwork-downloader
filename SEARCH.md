# Search System Documentation

This document explains how the image search system works in ODE Artwork Downloader and how to modify it for different use cases.

---

## Architecture Overview

The search system consists of three main components:

1. **Query Building** (`src/api/artwork.rs`) - Constructs search queries from disc information
2. **Search Execution** (`src/search/mod.rs`) - Fetches results from DuckDuckGo
3. **UI Integration** (`src/gui/app.rs`) - Displays results and handles user interaction

---

## Query Building System

### Location: `src/api/artwork.rs`

### Key Structure: `ArtworkSearchQuery`

```rust
pub struct ArtworkSearchQuery {
    pub title: String,           // Game title (e.g., "Final Fantasy VII")
    pub alt_title: Option<String>, // Alternative title from volume label
    pub region: Option<String>,   // Region code (USA, Europe, Japan)
    pub platform: Option<String>, // Platform (DOS, PC, Mac, Macintosh)
    pub year: Option<u32>,        // Release year
    pub search_suffix: String,    // Additional search terms
}
```

### Default Search Suffix

**Location**: Line 26 in `src/api/artwork.rs`

```rust
const DEFAULT_SUFFIX: &'static str = "\"jewel case\" art -site:ebay.com -\"playstation\" -\"xbox\" -\"nintendo\"";
```

**What it does**:
- **Includes**: `"jewel case"` and `art` - narrows results to jewel case artwork
- **Excludes**: 
  - `-site:ebay.com` - removes eBay listings
  - `-"playstation"` - filters out PlayStation results
  - `-"xbox"` - filters out Xbox results
  - `-"nintendo"` - filters out Nintendo results

**Why these exclusions?** These are intended for PC/DOS games, so console platforms are filtered.

---

## Modifying Search Behavior

### 1. Adding Platform-Specific Search Terms

**Example: Mac Games Should Include Macintosh Garden**

**Location**: `detect_platform_from_path()` function (line 126) and `build_query()` function (line 93)

**Current platform detection** (line 126-143):
```rust
fn detect_platform_from_path(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy().to_lowercase();
    
    if path_str.contains("/dos/") || path_str.contains("\\dos\\") {
        Some("DOS".to_string())
    } else if path_str.contains("/pc/") || path_str.contains("\\pc\\") {
        Some("PC".to_string())
    } else if path_str.contains("/macintosh/") || path_str.contains("\\macintosh\\") {
        Some("Macintosh".to_string())
    } else if path_str.contains("/mac/") || path_str.contains("\\mac\\") {
        Some("Mac".to_string())
    } else {
        None
    }
}
```

**How to add Mac-specific search terms:**

**Option A**: Modify `DEFAULT_SUFFIX` based on platform in `from_disc_info()` (line 43):

```rust
pub fn from_disc_info(info: &DiscInfo) -> Self {
    // ... existing code to get title, alt_title, etc ...
    
    let platform = detect_platform_from_path(&info.path);
    
    // Choose suffix based on platform
    let search_suffix = match platform.as_deref() {
        Some("Mac") | Some("Macintosh") => {
            "\"jewel case\" art site:macintoshgarden.org OR site:macintoshrepository.org"
        }
        _ => Self::DEFAULT_SUFFIX
    }.to_string();
    
    Self {
        title,
        alt_title,
        region: info.parsed_filename.region.clone(),
        platform,
        year: info.parsed_filename.year,
        search_suffix,  // Use platform-specific suffix
    }
}
```

**Option B**: Add a method to customize suffix per platform:

```rust
impl ArtworkSearchQuery {
    // Add this new method after line 82
    pub fn platform_specific_suffix(platform: Option<&str>) -> String {
        match platform {
            Some("Mac") | Some("Macintosh") => {
                "\"jewel case\" art (site:macintoshgarden.org OR site:macintoshrepository.org)"
                    .to_string()
            }
            Some("DOS") => {
                "\"jewel case\" art site:archive.org DOS games".to_string()
            }
            _ => Self::DEFAULT_SUFFIX.to_string(),
        }
    }
}
```

Then modify `from_disc_info()` to use it:
```rust
let search_suffix = Self::platform_specific_suffix(platform.as_deref());
```

---

### 2. Modifying the Search Suffix String

**Location**: Line 26 in `src/api/artwork.rs`

**To add terms** (must match):
```rust
const DEFAULT_SUFFIX: &'static str = "\"jewel case\" art \"PC game\" -site:ebay.com";
```

**To exclude additional sites**:
```rust
const DEFAULT_SUFFIX: &'static str = "\"jewel case\" art -site:ebay.com -site:pinterest.com -site:etsy.com";
```

**To search specific sites only**:
```rust
const DEFAULT_SUFFIX: &'static str = "\"jewel case\" art (site:mobygames.com OR site:archive.org)";
```

**To change artwork type**:
```rust
const DEFAULT_SUFFIX: &'static str = "\"box art\" OR \"cover art\" -site:ebay.com";
```

---

### 3. Adding Conditional Logic for Search Terms

**Example: Different search terms for different regions**

**Location**: Modify `build_query()` at line 93

**Current region handling** (line 109-118):
```rust
if let Some(ref region) = self.region {
    let region_term = match region.to_uppercase().as_str() {
        "USA" | "NTSC-U" => "USA",
        "EUROPE" | "PAL" | "PAL-E" => "Europe",
        "JAPAN" | "NTSC-J" => "Japan",
        _ => region.as_str(),
    };
    parts.push(region_term.to_string());
}
```

**To add region-specific suffixes:**

```rust
// After line 118, before parts.push(self.search_suffix.clone())

// Adjust suffix based on region
let final_suffix = if let Some(ref region) = self.region {
    match region.to_uppercase().as_str() {
        "JAPAN" | "NTSC-J" => {
            // For Japanese games, also search Japanese sites
            format!("{} site:gamefaqs.com \"Japan\"", self.search_suffix)
        }
        "EUROPE" | "PAL" | "PAL-E" => {
            // For European games, include PAL
            format!("{} \"PAL\"", self.search_suffix)
        }
        _ => self.search_suffix.clone()
    }
} else {
    self.search_suffix.clone()
};

parts.push(final_suffix);
```

---

### 4. Platform Detection - Adding New Platforms

**Location**: `detect_platform_from_path()` function (line 126)

**To add a new platform** (e.g., Dreamcast):

```rust
fn detect_platform_from_path(path: &Path) -> Option<String> {
    let path_str = path.to_string_lossy().to_lowercase();
    
    if path_str.contains("/dos/") || path_str.contains("\\dos\\") {
        Some("DOS".to_string())
    } else if path_str.contains("/pc/") || path_str.contains("\\pc\\") {
        Some("PC".to_string())
    } else if path_str.contains("/macintosh/") || path_str.contains("\\macintosh\\") {
        Some("Macintosh".to_string())
    } else if path_str.contains("/mac/") || path_str.contains("\\mac\\") {
        Some("Mac".to_string())
    } else if path_str.contains("/dreamcast/") || path_str.contains("\\dreamcast\\") {
        Some("Dreamcast".to_string())  // NEW PLATFORM
    } else {
        None
    }
}
```

Then combine with platform-specific suffix (see section 1).

---

### 5. Region Code Normalization

**Location**: `build_query()` function, line 109

**Current mapping**:
```rust
let region_term = match region.to_uppercase().as_str() {
    "USA" | "NTSC-U" => "USA",
    "EUROPE" | "PAL" | "PAL-E" => "Europe",
    "JAPAN" | "NTSC-J" => "Japan",
    _ => region.as_str(),
};
```

**To add more region codes**:
```rust
let region_term = match region.to_uppercase().as_str() {
    "USA" | "NTSC-U" | "US" | "AMERICA" => "USA",
    "EUROPE" | "PAL" | "PAL-E" | "EU" | "EUR" => "Europe",
    "JAPAN" | "NTSC-J" | "JP" | "JPN" => "Japan",
    "UK" | "UNITED KINGDOM" => "United Kingdom",
    "AUSTRALIA" | "AU" | "AUS" => "Australia",
    _ => region.as_str(),
};
```

---

## DuckDuckGo Search Implementation

### Location: `src/search/mod.rs`

### How the Search Works

1. **Token Retrieval** (`get_vqd_token()` - line 74):
   - Fetches DuckDuckGo search page
   - Extracts `vqd` token required for API calls
   - Token is hidden in page HTML

2. **Image Results Fetch** (`fetch_image_results()` - line 131):
   - Calls DuckDuckGo images API: `https://duckduckgo.com/i.js`
   - URL parameters:
     - `q` = search query
     - `vqd` = token from step 1
     - `o=json` = JSON response format
     - `l=us-en` = English language

3. **Result Parsing**:
   - Parses JSON response into `ImageResult` structs
   - Returns up to `max_results` images (default: 20)

### Modifying the Search Provider

**If DuckDuckGo API changes or you want to switch providers:**

**Current API call** (line 133-139):
```rust
let url = format!(
    "https://duckduckgo.com/i.js?l=us-en&o=json&q={}&vqd={}&f=,,,,,&p=1",
    urlencoding::encode(query),
    urlencoding::encode(vqd)
);
```

**To change language**:
```rust
"https://duckduckgo.com/i.js?l=ja-jp&o=json&q={}&vqd={}&f=,,,,,&p=1"  // Japanese
```

**To add safe search**:
```rust
"https://duckduckgo.com/i.js?l=us-en&o=json&kp=-2&q={}&vqd={}&f=,,,,,&p=1"  // kp=-2 = strict
```

---

## UI Integration

### Location: `src/gui/app.rs`

### Search Triggering

**Function**: `start_search()` - line 186

```rust
fn start_search(&mut self, query: &str) {
    let query = query.to_string();
    let (tx, rx) = mpsc::channel();
    
    self.search_in_progress = true;
    self.search_results.clear();
    
    thread::spawn(move || {
        let result = crate::search::search_images(&query, 20);  // max_results = 20
        let _ = tx.send(result);
    });
}
```

**To change max results**: Change `20` to desired number (line 195).

### Query Generation

**Function**: `update_search_query_from_disc()` - searches for this in the file

The UI generates the editable search query using:
```rust
let query = ArtworkSearchQuery::from_disc_info(info);
self.search_query_text = query.build_query();
```

---

## Testing Changes

### Unit Tests

Tests are located at the bottom of `src/api/artwork.rs` (line 201+).

**To test your modifications:**

1. Run existing tests:
```bash
cargo test artwork
```

2. Add new test cases:
```rust
#[test]
fn test_mac_game_search() {
    let mut info = DiscInfo::default();
    info.path = PathBuf::from("/Games/Mac/SimCity 2000.iso");
    
    let query = ArtworkSearchQuery::from_disc_info(&info);
    let search_str = query.build_query();
    
    assert!(search_str.contains("macintoshgarden"));
    assert_eq!(query.platform, Some("Mac".to_string()));
}
```

---

## Complete Example: Mac Games with Specific Sites

Here's a complete implementation for Mac games to search macintoshgarden.org:

### Step 1: Modify `src/api/artwork.rs` at line 43

Replace the `from_disc_info()` function:

```rust
pub fn from_disc_info(info: &DiscInfo) -> Self {
    let filename_title = info.parsed_filename.title.clone();

    let volume_title = info.volume_label.as_ref().and_then(|label| {
        if label.len() > 4 && !label.chars().all(|c| c.is_uppercase() || c == '_') {
            Some(crate::disc::normalize_volume_label(label))
        } else {
            None
        }
    });

    let (title, alt_title) = match volume_title {
        Some(ref vol) if vol.to_lowercase() != filename_title.to_lowercase() => {
            (filename_title.clone(), Some(vol.clone()))
        }
        Some(vol) => (vol, None),
        None => (filename_title, None),
    };

    let platform = detect_platform_from_path(&info.path);
    
    // **CUSTOM SUFFIX BASED ON PLATFORM**
    let search_suffix = match platform.as_deref() {
        Some("Mac") | Some("Macintosh") => {
            "\"jewel case\" art (site:macintoshgarden.org OR site:macintoshrepository.org OR site:archive.org)"
        }
        _ => Self::DEFAULT_SUFFIX
    }.to_string();

    Self {
        title,
        alt_title,
        region: info.parsed_filename.region.clone(),
        platform,
        year: info.parsed_filename.year,
        search_suffix,
    }
}
```

### Step 2: Test

```bash
cargo build
cargo run
```

Load a Mac game from a path containing `/Mac/` or `/Macintosh/` and verify the search query includes your custom sites.

---

## Quick Reference

| What to Modify | File | Line | Purpose |
|---------------|------|------|---------|
| Default search suffix | `src/api/artwork.rs` | 26 | Change base search terms |
| Platform detection | `src/api/artwork.rs` | 126 | Add new platforms |
| Platform-specific terms | `src/api/artwork.rs` | 43 | Different suffix per platform |
| Region normalization | `src/api/artwork.rs` | 109 | Add region code mappings |
| DuckDuckGo API URL | `src/search/mod.rs` | 133 | Change search provider params |
| Max search results | `src/gui/app.rs` | 195 | Number of results to fetch |

---

## Common Modifications

### Remove Console Exclusions

**Before**: `-"playstation" -"xbox" -"nintendo"`
**After**: Remove these from `DEFAULT_SUFFIX`

### Search Only Specific Sites

**Before**: Generic web search
**After**: `"jewel case" art (site:mobygames.com OR site:archive.org)`

### Add CD Longbox Support

**Before**: `"jewel case" art`
**After**: `("jewel case" OR "longbox" OR "CD case") art`

### Filter by Image Quality

Add to suffix: `imagesize:800x800` (DuckDuckGo syntax)

---

## Troubleshooting

**Search returns no results:**
- Check if `vqd` token extraction is working (logs show "Found vqd token")
- Verify search query isn't too restrictive
- Test query manually at https://duckduckgo.com

**Wrong platform detected:**
- Verify path contains platform folder name (case-insensitive)
- Add debug logging in `detect_platform_from_path()`

**Mac sites not appearing:**
- Ensure `search_suffix` includes `site:` operators
- DuckDuckGo may need multiple searches if sites are small

---

## Notes

- Search queries are **editable in the UI** before searching
- User can click "Reset" to regenerate query from disc info
- Manual URL field allows bypassing search entirely
- Right-click on results opens image in browser for manual download
