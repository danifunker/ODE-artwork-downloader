# Plan: CD Image Hashing and Redump Database Integration

## Executive Summary

This document analyzes the feasibility of implementing disc image hash verification against Redump databases for your ODE-artwork-downloader project. The key finding is that **yes, we can extract and verify data track hashes from consolidated BIN/CUE, CHD, and ISO files**, but there are important technical considerations for each format.

---

## 1. Which Hash is Best for Data Track Verification?

### Recommendation: **SHA1** (with MD5 as secondary)

| Hash | Speed | Collision Resistance | Redump Support | Recommendation |
|------|-------|---------------------|----------------|----------------|
| CRC32 | Fastest | Poor (32-bit) | ✅ Yes | Useful for quick pre-filtering |
| MD5 | Fast | Broken for crypto, fine for integrity | ✅ Yes | Good secondary identifier |
| SHA1 | Moderate | Good for integrity | ✅ Yes | **Primary identifier** |

**Why SHA1?**
- Redump uses SHA1 as the authoritative hash
- 160-bit output is sufficient for uniqueness across ~50,000 disc images
- Already have the `sha1` crate in your dependencies
- Industry standard for disc verification (also used by TOSEC, No-Intro)

**Implementation approach:**
- Calculate SHA1 for verification/lookup
- Optionally calculate CRC32 first as a quick pre-filter (if you have a CRC index, you can rule out non-matches before computing full SHA1)

---

## 2. Comparing ISOs to BIN Hashes

### The Core Problem

**BIN hash ≠ ISO hash** for the same disc. Here's why:

```
BIN (Raw Sector - 2352 bytes):
┌──────────────────────────────────────────────────────────────┐
│ Sync (12) │ Header (4) │ USER DATA (2048) │ EDC/ECC (288)   │
└──────────────────────────────────────────────────────────────┘

ISO (Cooked Sector - 2048 bytes):
┌──────────────────────────────────────────────────────────────┐
│                     USER DATA (2048)                         │
└──────────────────────────────────────────────────────────────┘
```

### Solution: Extract User Data from BIN

For a data track in MODE1/2352 format:
1. Read each 2352-byte sector from BIN
2. Skip to offset 16 (after sync + header)
3. Read 2048 bytes of user data
4. Hash only the user data portion

**The extracted user data hash WILL match the ISO hash** if:
- Both represent the same disc
- The ISO was created from the same source

### Cross-Format Hash Comparison Matrix

| Source Format | Can Match ISO? | Can Match BIN Track? | Notes |
|--------------|----------------|---------------------|-------|
| ISO file | ✅ Direct | ✅ Extract user data | Straightforward |
| BIN (single track) | ✅ Extract user data | ✅ Direct | Need to strip sync/header/EDC |
| BIN (multi-track) | ✅ Extract Track 01 user data | ✅ Extract Track 01 | Use CUE track boundaries |
| CHD | ✅ Extract & strip | ✅ Extract track | Decompress first |

### Practical Implications

**For matching against Redump:**
- Redump stores hashes of **original track files** (raw 2352-byte sectors)
- If user has ISO: Convert to "what-if-it-were-raw" hash, OR maintain a separate ISO hash database
- If user has BIN: Hash matches directly (for same track structure)
- If user has CHD: Extract track, hash raw bytes

**Recommendation:** Store both raw-track hashes (for BIN/CHD matching) AND user-data-only hashes (for ISO matching) in your database.

---

## 3. Should We Create a Database to Distribute with the App?

### Yes - Recommended Approach

**Option A: Embedded SQLite Database (Recommended)**
```
pros:
- Fast indexed lookups
- Support multiple hash types (CRC, MD5, SHA1)
- Small footprint (~10-20MB for major platforms)
- Works offline

cons:
- Need to update app or download updates
- SQLite dependency (already lightweight)
```

**Option B: Compressed DAT Files**
```
pros:
- Matches Redump's distribution format
- User can update independently

cons:
- Slower lookups (need to parse XML)
- Larger file size uncompressed
```

**Option C: Hybrid (Best of Both)**
```
- Ship SQLite database with app
- Allow loading additional DAT files for updates
- Parse DAT → merge into local SQLite
```

### Proposed Database Schema

```sql
-- Core game information
CREATE TABLE games (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    category TEXT,
    platform TEXT NOT NULL  -- 'psx', 'ps2', 'dc', 'saturn', etc.
);

-- Track hashes from Redump (for BIN/CHD matching)
CREATE TABLE tracks (
    id INTEGER PRIMARY KEY,
    game_id INTEGER REFERENCES games(id),
    track_number INTEGER NOT NULL,
    track_name TEXT,
    size INTEGER,
    crc32 TEXT,
    md5 TEXT,
    sha1 TEXT,
    is_data_track BOOLEAN DEFAULT FALSE
);

-- Indexes for fast lookup
CREATE INDEX idx_tracks_sha1 ON tracks(sha1);
CREATE INDEX idx_tracks_md5 ON tracks(md5);
CREATE INDEX idx_tracks_crc32 ON tracks(crc32);

-- For ISO matching (user data only hashes)
CREATE TABLE iso_hashes (
    id INTEGER PRIMARY KEY,
    game_id INTEGER REFERENCES games(id),
    sha1_user_data TEXT,  -- Hash of extracted 2048-byte sectors
    md5_user_data TEXT
);

CREATE INDEX idx_iso_sha1 ON iso_hashes(sha1_user_data);
```

### Database Size Estimates

| Platform | Games | Tracks (est.) | DB Size (est.) |
|----------|-------|---------------|----------------|
| PlayStation | ~8,000 | ~12,000 | ~3 MB |
| PlayStation 2 | ~10,000 | ~15,000 | ~4 MB |
| Sega Saturn | ~2,500 | ~5,000 | ~1.5 MB |
| Sega CD | ~600 | ~2,000 | ~0.5 MB |
| PC (CD) | ~15,000 | ~20,000 | ~5 MB |
| **Total** | ~36,000 | ~54,000 | **~15 MB** |

---

## 4. Extracting Data from Redump DAT XML

### Current XML Structure (Your Examples)

**Single-track data disc (Deus Ex):**
```xml
<game name="Deus Ex (USA)">
    <rom name="Deus Ex (USA).cue" size="79" crc="a6415534" md5="..." sha1="..."/>
    <rom name="Deus Ex (USA).bin" size="783477072" crc="9c096c85" md5="5c63c7f81a24473a643ee3960c27ffa0" sha1="a6f72f17fc0f9fd0196d9d159986b7bde838856d"/>
</game>
```

**Multi-track disc (MechWarrior 2):**
```xml
<game name="MechWarrior 2 - 31st Century Combat (USA)">
    <rom name="... (Track 01).bin" ... md5="1267be089821cfb5922c4d027f892991" sha1="a885a11c44c54620d88bb8da94a58720216919a9"/>
    <rom name="... (Track 02).bin" ... />  <!-- Audio track -->
    <!-- ... more tracks ... -->
</game>
```

### Parsing Strategy

```rust
// Pseudocode for DAT parsing
fn parse_redump_dat(xml: &str) -> Vec<GameEntry> {
    for game in xml.games {
        let mut tracks = Vec::new();

        for rom in game.roms {
            if rom.name.ends_with(".bin") {
                let track_num = extract_track_number(&rom.name);  // None for single-track
                let is_data = track_num.is_none() || track_num == Some(1);

                tracks.push(TrackEntry {
                    track_number: track_num.unwrap_or(1),
                    is_data_track: is_data,
                    size: rom.size,
                    crc32: rom.crc,
                    md5: rom.md5,
                    sha1: rom.sha1,
                });
            }
        }

        // Store game with tracks
    }
}

fn extract_track_number(filename: &str) -> Option<u32> {
    // Match "(Track XX)" pattern
    let re = regex!(r"\(Track (\d+)\)\.bin$");
    re.captures(filename).map(|c| c[1].parse().unwrap())
}
```

### Identifying the Data Track

**Rules:**
1. **Single BIN file** → It's the data track (could be data-only or mixed-mode consolidated)
2. **Multiple BIN files** → Track 01 is typically data, Track 02+ are typically audio
3. **File size heuristic**: Audio tracks are ~17.6 MB per minute of audio (2352 × 75 × 60)
4. **Track type from CUE**: Can parse CUE in DAT to confirm MODE1 vs AUDIO

**For consolidated BIN/CUE (USBODE format):**
- The single BIN contains all original tracks sequentially
- Track boundaries are defined in the CUE file
- You CAN extract each original track and compute its hash

---

## 5. Extracting Original Track Hashes from Consolidated BIN/CUE

### Yes, This Is Possible!

Your app already has the infrastructure in `src/disc/bincue.rs`. Here's how to extract Track 01 (data track) hash:

```rust
// Conceptual implementation
fn hash_track_from_consolidated_bin(
    cue_path: &Path,
    track_number: u32,
) -> Result<(String, String), Error> {  // Returns (md5, sha1)
    let disc_info = parse_cue(cue_path)?;

    // Find the track
    let track = disc_info.tracks.iter()
        .find(|t| t.track_number == track_number)
        .ok_or(Error::TrackNotFound)?;

    // Calculate track boundaries
    let start_byte = track.start_frame * track.sector_size as u64;
    let end_byte = match disc_info.tracks.iter()
        .find(|t| t.track_number == track_number + 1) {
            Some(next) => next.start_frame * next.sector_size as u64,
            None => file_size,  // Last track goes to end
        };

    // Open BIN file and read track data
    let mut file = File::open(&bin_path)?;
    file.seek(SeekFrom::Start(start_byte))?;

    let mut md5_hasher = Md5::new();
    let mut sha1_hasher = Sha1::new();

    let bytes_to_read = end_byte - start_byte;
    let mut buffer = vec![0u8; 65536];  // 64KB buffer
    let mut remaining = bytes_to_read;

    while remaining > 0 {
        let to_read = std::cmp::min(remaining, buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..to_read])?;

        md5_hasher.update(&buffer[..to_read]);
        sha1_hasher.update(&buffer[..to_read]);

        remaining -= to_read as u64;
    }

    Ok((
        hex::encode(md5_hasher.finalize()),
        hex::encode(sha1_hasher.finalize()),
    ))
}
```

### The Catch: Track Consolidation Method Matters

**If USBODE consolidates tracks by simple concatenation:**
- ✅ Track boundaries are preserved
- ✅ Hashes will match Redump

**If USBODE applies any transformation:**
- ❌ Hashes won't match
- Need to know the exact consolidation algorithm

**Verification test:** Take a multi-track BIN/CUE from Redump, consolidate it with USBODE's tool, extract Track 01, and compare hashes. If they match, you're good!

---

## 6. CHD File Hash Extraction

### Yes, Also Possible!

Your `src/disc/chd.rs` already extracts track metadata. Extend it:

```rust
fn hash_track_from_chd(
    chd_path: &Path,
    track_number: u32
) -> Result<(String, String), Error> {
    let mut chd = chd::Chd::open(&chd_path)?;
    let tracks = parse_cht2_metadata(&chd)?;

    let track = tracks.iter()
        .find(|t| t.track_number == track_number)
        .ok_or(Error::TrackNotFound)?;

    let mut md5_hasher = Md5::new();
    let mut sha1_hasher = Sha1::new();

    // Read all frames for this track
    for frame in track.start_frame..(track.start_frame + track.frame_count) {
        let sector_data = read_chd_sector(&mut chd, frame, track.sector_size)?;
        md5_hasher.update(&sector_data);
        sha1_hasher.update(&sector_data);
    }

    Ok((
        hex::encode(md5_hasher.finalize()),
        hex::encode(sha1_hasher.finalize()),
    ))
}
```

### CHD Hash Compatibility

**CHD stores raw sector data** (2352 bytes for CD-ROM), so:
- ✅ Extracted track hash = Original BIN track hash
- ✅ Can match against Redump

**Important:** CHD uses lossless compression, so data integrity is preserved.

---

## 7. ISO File Hash Extraction

### Simplest Case

```rust
fn hash_iso_file(iso_path: &Path) -> Result<(String, String), Error> {
    let mut file = File::open(iso_path)?;

    let mut md5_hasher = Md5::new();
    let mut sha1_hasher = Sha1::new();

    let mut buffer = vec![0u8; 65536];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 { break; }

        md5_hasher.update(&buffer[..bytes_read]);
        sha1_hasher.update(&buffer[..bytes_read]);
    }

    Ok((
        hex::encode(md5_hasher.finalize()),
        hex::encode(sha1_hasher.finalize()),
    ))
}
```

### ISO ↔ BIN Hash Comparison

**Problem:** ISO contains only user data (2048-byte sectors), BIN contains raw data (2352-byte sectors).

**Solutions:**

1. **For ISO → Redump matching:**
   - Redump doesn't typically have ISO hashes
   - Need to build your own ISO hash database
   - OR: Reconstruct raw sectors from ISO (add sync/header/EDC) and hash

2. **For BIN → ISO matching:**
   - Extract user data from BIN (skip first 16 bytes of each sector)
   - Hash only the 2048-byte user data portions
   - Compare to ISO hash

**Recommendation:** Maintain a separate index of "user data only" hashes for ISO matching. You can compute these from Redump BINs.

---

## 8. Implementation Roadmap

### Phase 1: Core Hashing Infrastructure
1. Add `md5` crate to dependencies
2. Create `src/disc/hasher.rs` module
3. Implement `hash_file()` for whole-file hashing
4. Implement `hash_track()` for track-specific hashing

### Phase 2: Database Creation Tool
1. Create `src/database/mod.rs` module
2. Add `rusqlite` dependency for SQLite
3. Parse Redump DAT files (already have `src/api/redump.rs`)
4. Build SQLite database with game/track tables
5. Create indexes for fast hash lookups

### Phase 3: Hash Lookup Integration
1. On disc scan, calculate SHA1 of Track 01 / whole file
2. Look up hash in database
3. If found, return game info (name, platform, category)
4. Fall back to existing filename/volume label methods

### Phase 4: ISO Hash Database (Optional)
1. For each Redump entry, calculate "user data only" hash
2. Store in separate table for ISO matching
3. Alternatively, offer a rebuild tool that converts existing BINs

---

## 9. Questions to Resolve

Before implementing, we should clarify:

1. **USBODE Consolidation Method:** How exactly does USBODE combine multi-track BIN/CUEs? Is it byte-identical concatenation?

2. **Target Platforms:** Which platforms/DAT files should be included? All Redump platforms or a subset?

3. **Update Strategy:** How should database updates be distributed?
   - Bundle new DB with app releases?
   - Downloadable update files?
   - Let users load their own DAT files?

4. **ISO Hash Priority:** How important is ISO matching? Building a user-data-only hash database requires processing all Redump BINs.

5. **Performance Targets:**
   - Acceptable hash calculation time per disc?
   - Database lookup speed requirements?

---

## 10. Summary

| Question | Answer |
|----------|--------|
| Best hash for data tracks? | **SHA1** (Redump standard), with MD5 as secondary |
| ISO vs BIN hash comparison? | Different by design; need to extract user data from BIN for comparison |
| Create distributable database? | **Yes** - SQLite recommended (~15MB for major platforms) |
| Extract hashes from consolidated BIN/CUE? | **Yes** - track boundaries preserved in CUE file |
| Extract hashes from CHD? | **Yes** - CHD preserves raw sector data |
| Extract hashes from ISO? | **Yes** - straightforward whole-file hash |

The implementation is feasible and would significantly improve game identification accuracy.
