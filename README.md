# ODE Artwork Downloader

A cross-platform GUI application that automatically identifies CD/DVD disc images and downloads appropriate cover art for the USBODE project.

## Overview

ODE Artwork Downloader streamlines the process of finding and preparing cover art for your disc image collection. It reads disc images, extracts game information, searches for jewel case artwork online, and converts images to the exact specifications required by USBODE devices (240x240 baseline JPEG).

## Features

### Disc Image Support
- **ISO/Toast** - Standard ISO 9660 disc images and macOS Toast images
- **CHD** - MAME Compressed Hunks of Data format
- **BIN/CUE** - Raw binary with cue sheet
- **MDS/MDF** - Media Descriptor Sidecar format (not implemented yet)

### Automatic Game Detection
- Reads ISO 9660 volume labels for accurate game identification
- Falls back to filename parsing for HFS/HFS+ Mac discs
- Extracts region codes (USA, Europe, Japan, etc.)
- Detects year from filename when present
- Identifies platform from folder path (DOS, PC, Mac, Macintosh)

### Smart Image Search
- Searches DuckDuckGo Images for "jewel case" artwork
- Generates search queries using both filename and volume label (OR search)
- Filters for square aspect ratio images
- Excludes common noise sources (eBay, etc.)
- Results sorted by aspect ratio (closest to square first)
- Editable search query for manual refinement

### Image Processing
- Automatic center-crop for non-square images
- Resize to 240x240 pixels using Lanczos3 filtering
- Baseline JPEG output (90% quality)
- Preserves correct naming convention (same name as disc image with .jpg extension)

### User Interface
- Drag-and-drop disc images to scan
- Drag-and-drop artwork images to convert (for manual downloads)
- Live image preview before downloading
- Right-click context menu to copy URLs or open in browser
- Manual URL input for pasting image links directly
- Log window for detailed operation history

## Usage

### Basic Workflow

1. **Load a disc image** - Drag and drop a disc image file onto the application, or click "Browse" to select one
2. **Review disc info** - The application displays detected volume label, format, and filesystem
3. **Search for artwork** - Click "Search" to find jewel case artwork online
4. **Preview results** - Click on search results to preview images
5. **Download** - Click "Download & Save" to convert and save the artwork

### Manual Image Workflow

If automatic download fails (403 errors, etc.):

1. Right-click a search result and select "Open in browser"
2. Download the image manually in your browser
3. Drag and drop the downloaded image onto the application
4. The image will be automatically converted and saved with the correct filename

### Search Query Customization

The search query is editable - modify it before searching to:
- Add game subtitles or alternate names
- Specify a particular region or platform
- Remove terms that produce poor results

Click "Reset" to restore the auto-generated query.

## Limitations

- **HFS/HFS+ discs** - Mac-formatted discs cannot be fully read; the application falls back to filename-only identification
- **MDS/MDF support** - Currently limited to filename parsing only
- **Search provider** - Uses DuckDuckGo only; some images may be blocked by source websites (403 errors)
- **Image detection** - Search results may include non-artwork images; manual review recommended
- **No batch processing** - Currently processes one disc at a time

## Technical Details

### Architecture

The application is built in Rust using:
- **eframe/egui** - Immediate-mode GUI framework
- **image** - Image decoding and processing
- **reqwest** - HTTP client for fetching images
- **regex** - Filename parsing and pattern matching

### Disc Reading

ISO 9660 Primary Volume Descriptors are read directly from disc images to extract volume labels. CHD files are decompressed on-the-fly to access the underlying ISO data. BIN/CUE files are parsed to locate data tracks.

### Search Implementation

The application queries DuckDuckGo's image search API:
1. Fetches the search page to obtain a `vqd` token
2. Queries `/i.js` endpoint for JSON results
3. Parses results including image URLs, dimensions, and titles

### Image Export Pipeline

1. Fetch image from URL (or read from local file)
2. Decode image format (JPEG, PNG, GIF, WebP, BMP)
3. Center-crop to square if needed
4. Resize to 240x240 using Lanczos3 interpolation
5. Encode as baseline JPEG (quality 90, no ICC profile)
6. Save with same base name as disc image

## Building from Source

```bash
# Clone the repository
git clone https://github.com/yourusername/ODE-artwork-downloader.git
cd ODE-artwork-downloader

# Build release version
cargo build --release

# Run
cargo run --release
```

### Dependencies

- Rust 1.70 or later
- System dependencies for GUI (platform-specific)

## License

[Add your license here]

## Acknowledgments

- Built for use with USBODE/MODE optical drive emulators
- Inspired by the need to streamline cover art preparation for large disc collections
