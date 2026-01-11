# CD Cover Art Downloader Tool - Project Brief

## Project Overview
Create a cross-platform GUI application in Rust that automatically identifies CD/DVD disc images and downloads appropriate jewel case cover art for the USBODE (USB Optical Drive Emulator) project.

## Target Platforms
- Windows: x86, x64, ARM
- Linux: x64, ARM
- macOS: x64, ARM (Apple Silicon)

## Supported Disc Image Formats
- ISO (ISO 9660)
- BIN/CUE
- CHD (MAME Compressed Hunks of Data)
- MDS/MDF

## Supported Filesystem Types
- ISO 9660 (standard)
- Joliet (Unicode extensions)
- UDF (Universal Disk Format)
- HFS (Hierarchical File System)
- HFS+ (Mac OS Extended)

---

## Implementation Status

### Phase 1: MVP

| Feature | Status | Notes |
|---------|--------|-------|
| Project setup (Cargo.toml, structure) | Done | |
| Core data types (formats, filesystems) | Done | `src/disc/formats.rs` |
| ISO9660 PVD reading | Done | `src/disc/iso9660.rs` |
| Filename parser with regex | Done | `src/disc/identifier.rs` |
| Basic GUI with egui | Done | `src/gui/app.rs` |
| File picker and drag-drop | Done | |
| Disc info display | Done | |
| CHD support | Pending | Stub implemented, needs CHD library |
| BIN/CUE support | Pending | Stub implemented |
| MDS/MDF support | Pending | Stub implemented |

### Phase 2: API Integration & Cover Art

| Feature | Status | Notes |
|---------|--------|-------|
| HTTP client wrapper | Pending | |
| Redump integration | Pending | DAT file parsing |
| MAME integration | Pending | Software list parsing |
| IGDB integration | Pending | OAuth required |
| MobyGames cover art | Pending | Primary cover source |
| Image download/resize | Pending | 240x240 JPEG output |
| GUI search results | Pending | |

### Phase 3: Batch Processing & Polish

| Feature | Status | Notes |
|---------|--------|-------|
| Batch folder scanning | Pending | |
| Skip existing covers | Pending | |
| CSV export | Pending | |
| Multi-disc handling | Pending | |
| Disc number overlay | Pending | User-configurable |
| Settings/configuration | Pending | |

---

## Design Decisions

### Game Identification Flow
1. Extract disc info (volume label, serial numbers from filename/metadata)
2. Query identification sources in order:
   - **Redump** - Serial number matching (highest accuracy)
   - **MAME** - Game database lookup
   - **IGDB** - Fallback game metadata
3. Use identified game info to search for cover art

### Cover Art Retrieval
- **Primary**: MobyGames (comprehensive cover art database with API)
- Search using game title + platform from identification step
- Download and resize to 240x240 JPEG for USBODE

### CHD Handling Decision
Include in Phase 1. Approach TBD:
1. **chdman subprocess** - Shell out to chdman tool (simple, requires chdman installed)
2. **Pure Rust** - Use or create pure Rust CHD reader (preferred for portability)

### File Naming Convention
Output: `{original_filename}.jpg` (e.g., `game.chd` -> `game.jpg`)

---

## Core Functionality

### Stage 1: Disc Identification (Multi-Approach)
1. **Volume Label Extraction** (Primary method)
   - Read sector 16 for ISO9660 Primary Volume Descriptor
   - Extract volume ID from offset 40 (32 bytes)
   - Handle Joliet/UDF extensions
   - For HFS/HFS+: Use appropriate libraries or fallback methods

2. **Filename Parsing** (Fallback method with fuzzy logic)
   - Extract game title from filename
   - Parse and remove common patterns:
     - Region codes: `(USA)`, `(Europe)`, `(Japan)`, etc.
     - Disc numbers: `(Disc 1)`, `(CD 2)`, etc.
     - Serial numbers: `[SLUS-12345]`, `[SCUS-94900]`, etc.
     - Version info: `v1.2`, `Rev A`, etc.
   - Normalize separators (underscores, dashes to spaces)
   - Clean up extra whitespace

3. **CHD Specific Handling**
   - Read sectors directly without extracting full CHD
   - Read only necessary sectors (e.g., sector 16 for ISO9660 PVD)
   - Check CHD metadata for embedded game information if available
   - Avoid creating temporary files

### Stage 2: Cover Art Lookup
1. Query game database APIs with extracted title
2. API priority:
   - Redump (serial matching)
   - MAME (software lists)
   - IGDB (game metadata)
3. Cover art from MobyGames
4. Return multiple results if available
5. Provide confidence scoring (high: volume label, medium: CHD metadata, low: filename)

### Stage 3: Image Processing
1. Download cover art image
2. Resize to 240x240 pixels
3. Convert to JPEG format
4. Save with same name as disc image (different extension)

---

## Technical Stack

### Language & Framework
- **Rust** (2021 edition)
- **egui/eframe** for GUI (immediate mode, cross-platform, simple)

### Current Dependencies
```toml
[dependencies]
eframe = "0.30"
egui = "0.30"
egui_extras = { version = "0.30", features = ["image"] }
image = "0.25"
reqwest = { version = "0.12", features = ["blocking", "json"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
regex = "1.10"
thiserror = "2.0"
rfd = "0.15"
log = "0.4"
env_logger = "0.11"
```

---

## Project Structure
```
ode-artwork-downloader/
├── Cargo.toml
├── PROJECT.md
├── LICENSE
├── src/
│   ├── main.rs              # Application entry point, GUI setup
│   ├── lib.rs               # Library root
│   ├── disc/
│   │   ├── mod.rs           # Disc module exports
│   │   ├── formats.rs       # Format and filesystem enums
│   │   ├── reader.rs        # Unified disc reader interface
│   │   ├── iso9660.rs       # ISO9660 PVD parsing
│   │   └── identifier.rs    # Filename parsing, title extraction
│   ├── gui/
│   │   ├── mod.rs           # GUI module exports
│   │   └── app.rs           # Main application state and UI
│   ├── api/                 # (Phase 2)
│   │   ├── mod.rs
│   │   ├── client.rs
│   │   ├── redump.rs
│   │   ├── mame.rs
│   │   ├── igdb.rs
│   │   └── mobygames.rs
│   └── image/               # (Phase 2)
│       ├── mod.rs
│       └── processor.rs
└── .github/
    └── workflows/           # (TODO: CI/CD)
```

---

## GUI Features

### Main Window
1. **File Input Section**
   - [x] File browser button for selecting disc image
   - [x] Text field showing selected file path
   - [x] Drag-and-drop support

2. **Disc Information Display**
   - [x] Detected volume label
   - [x] Parsed filename information
   - [x] Confidence level indicator (color-coded)
   - [x] Detected format and filesystem type
   - [x] Cover art status (exists/not found)

3. **Search Results Section**
   - [ ] List of found cover art options
   - [ ] Preview thumbnails
   - [ ] Game title, platform, region information
   - [ ] Selection mechanism

4. **Action Buttons**
   - [x] "Browse..." button (file picker)
   - [ ] "Search Cover Art" button (placeholder)
   - [ ] "Download Selected" button (placeholder)
   - [ ] Progress indicators

5. **Status/Log Area**
   - [x] Operation status messages
   - [x] Error reporting (color-coded)
   - [x] Success confirmations

---

## Build Configuration
```toml
[profile.release]
opt-level = "z"        # Optimize for size
lto = true             # Link-time optimization
codegen-units = 1      # Better optimization
strip = true           # Strip debug symbols
```

## Cross-Compilation Notes
- Use `cargo build --release --target <target-triple>`
- Windows builds may require mingw toolchain
- macOS cross-compilation requires osxcross
- Test on all target platforms before release

---

## Key Technical Considerations

1. **CHD Reading Strategy**: Read specific sectors directly rather than extracting entire disc images. This saves disk space and time.

2. **Error Handling**: Robust error handling for:
   - Corrupt or unsupported disc images
   - Network failures during API calls
   - Missing or invalid cover art
   - File I/O errors

3. **Performance**:
   - Async API calls where appropriate
   - Caching of API responses
   - Minimal memory footprint for large disc images

4. **User Experience**:
   - Clear progress indicators
   - Helpful error messages
   - Sensible defaults
   - Keyboard shortcuts

---

## Success Criteria
- Successfully extracts volume labels from all supported formats
- Accurately identifies games with >80% success rate
- Downloads and converts cover art automatically
- Single-binary deployment on all platforms
- Binary size under 20MB
- Startup time under 2 seconds

---

## Resolved Questions
1. **Image format for USBODE**: 240 x 240px JPEG images
2. **Batch processing**: Yes, supported
3. **Naming convention**: Same as source file with .jpg extension
4. **API caching**: Yes (duration TBD)
5. **Manual override**: Yes, implement later

---

## Project Management

### TODO
- [ ] Add GitHub Actions for cross-platform builds
- [ ] Implement CHD reading (investigate pure Rust options)
- [ ] Implement API integrations (Redump, MAME, IGDB, MobyGames)
- [ ] Add batch processing mode
- [ ] Add CSV export functionality

### Multi-disc Handling
- Detect multi-disc games from filename patterns
- Option for disc number overlay on cover art
- User-configurable overlay settings

### CSV Export Format
```csv
fullpath,lookup_source,lookup_query,image_url,filesize
/path/to/game.iso,volume_label,GAME_TITLE,https://...,12345
```

### Batch Processing
- Scan folder for supported disc image formats
- Skip files that already have associated .jpg covers
- Progress tracking for batch operations

---

## Getting Started
```bash
# Clone the repository
git clone https://github.com/dani/ODE-artwork-downloader
cd ODE-artwork-downloader

# Run development build
cargo run

# Build release for current platform
cargo build --release
```
