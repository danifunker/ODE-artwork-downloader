# TODO: HFS/HFS+ File Reading Support

## Current Status

| Filesystem | Directory Listing | File Reading | Location |
|------------|------------------|--------------|----------|
| **ISO 9660** | ✅ Working | ✅ Working | `src/disc/browse/iso9660_fs.rs` |
| **HFS+** | ✅ Working | ⚠️ Partial | `src/disc/browse/hfsplus_fs.rs` |
| **HFS classic** | ✅ Working | ❌ Not implemented | `src/disc/browse/hfs_fs.rs` |

## Problem Description

When browsing HFS or HFS+ volumes, users can see the file tree but clicking on files to view content returns:
```
Error: Failed to read file: Unsupported filesystem
```

---

## HFS+ Implementation Details

### What's Already Working (hfsplus_fs.rs)

- Volume header parsing (signature, block size, catalog extent)
- B-tree traversal for catalog file
- Directory listing via `list_directory_by_cnid()`
- UTF-16 BE filename decoding
- Basic file reading for **single-extent files only**

### What's Missing

1. **Multi-extent file support** (lines 495-496):
   ```rust
   // Read from first extent (simplified - real impl would handle multiple extents)
   let extent = &fork.extents[0];
   ```
   - Currently only reads the first extent descriptor
   - Large or fragmented files have multiple extents
   - Need to iterate through all 8 inline extents in the fork data

2. **Extent Overflow B-tree**:
   - Files with >8 extents store additional extents in the Extents Overflow file
   - Would need to parse the extents B-tree (similar structure to catalog B-tree)
   - Location: Volume header bytes 176-255 (extents file fork data)

### HFS+ File Record Structure (for reference)

```
Offset  Size  Description
------  ----  -----------
0       2     Record type (2 = file)
2       2     Flags
4       4     Reserved
8       4     CNID (file ID) <-- we use this
12      4     Create date
16      4     Content mod date
20      4     Attribute mod date
24      4     Access date
28      4     Backup date
32      16    Permissions
48      16    Finder info
64      16    Extended finder info
80      4     Text encoding
84      4     Reserved
88      80    Data fork <-- we parse this
168     80    Resource fork

Fork Data Structure (80 bytes):
0       8     Logical size
8       4     Clump size
12      4     Total blocks
16      64    Extents (8 x 8 bytes each)

Each Extent (8 bytes):
0       4     Start block
4       4     Block count
```

### Estimated Effort: 2-4 hours

1. Parse all 8 inline extents (not just first one): ~1 hour
2. Calculate correct offsets across multiple extents: ~1 hour
3. (Optional) Extent overflow file support: ~2-3 hours additional

---

## HFS Classic Implementation Details

### What's Already Working (hfs_fs.rs)

- Master Directory Block (MDB) parsing
- B-tree traversal for catalog file
- Directory listing via `list_directory_by_id()`
- MacRoman to UTF-8 filename conversion

### What's Missing

The `read_file()` method returns `Unsupported` (lines 330-335):
```rust
fn read_file(&mut self, _entry: &FileEntry) -> Result<Vec<u8>, FilesystemError> {
    // HFS file reading requires finding the file's extent info
    // This is a simplified placeholder - full implementation would
    // traverse the catalog to find extent records
    Err(FilesystemError::Unsupported)
}
```

### Implementation Requirements

1. **Parse file extent info from catalog record**:
   - File record contains first 3 extent descriptors for data fork
   - Each extent: start block (2 bytes) + block count (2 bytes)
   - Located at offset 18-29 in the file record data

2. **Calculate allocation block offsets**:
   ```
   physical_offset = partition_offset
                   + (alloc_block_start * 512)
                   + (extent_start_block * alloc_block_size)
   ```

3. **Handle extent overflow**:
   - Files with >3 extents store additional extents in Extents Overflow B-tree
   - Key: file ID + fork type + starting block
   - Located via MDB fields at bytes 148-149 (first extent) and 150-151 (length)

### HFS File Record Structure (for reference)

```
Offset  Size  Description
------  ----  -----------
0       1     Record type (2 = file)
1       1     Reserved
2       4     Flags + file type
6       16    Finder info
22      4     File ID <-- we have this as entry.location
26      2     Data fork first alloc block
28      4     Data fork logical size (EOF)
32      4     Data fork physical size
36      2     Resource fork first alloc block
38      4     Resource fork logical size
42      4     Resource fork physical size
46      4     Create date
50      4     Modify date
54      4     Backup date
58      16    Extended finder info
74      2     Clump size
76      12    Data fork extents (3 x 4 bytes)
88      12    Resource fork extents (3 x 4 bytes)

Extent Descriptor (4 bytes):
0       2     Start allocation block
2       2     Block count
```

### Estimated Effort: 6-9 hours

1. Parse extent info from file record: ~2-3 hours
2. Calculate allocation block physical offsets: ~2-3 hours
3. Extent overflow B-tree support: ~2-3 hours

---

## External Crate Option: hfsplus-rs

### Repository
- URL: https://github.com/penguin359/hfsplus-rs
- Author: Loren M. Lang
- Last activity: ~7 years ago
- Not published on crates.io

### Features
- `HFSVolume::load()` - open volumes
- `get_path()` / `get_children_id()` - directory listing
- `Fork` struct with `Read` trait - full file reading with extent support

### Integration Challenges

1. **FUSE dependency**: Depends on `fuse` crate (Linux-only), would break macOS/Windows
   - Would need to fork and remove FUSE features, or make them optional

2. **Read+Seek interface**: Expects standard file handle
   - Our `SectorReader` abstraction (for CHD/BIN-CUE support) is different
   - Would need wrapper struct implementing Read+Seek that delegates to SectorReader:
   ```rust
   struct SectorReaderAdapter {
       reader: Box<dyn SectorReader>,
       position: u64,
   }

   impl Read for SectorReaderAdapter { ... }
   impl Seek for SectorReaderAdapter { ... }
   ```

3. **Rust edition**: Uses Rust 2018, may need updates for compatibility

### Verdict
Probably not worth it - our implementation is close for HFS+, and no crate exists for HFS classic anyway.

---

## Recommended Implementation Order

1. **HFS+ multi-extent support** (2-4 hours)
   - Highest impact, fixes most HFS+ file reading
   - Builds on existing working code

2. **HFS classic file reading** (6-9 hours)
   - Reuse B-tree traversal logic from directory listing
   - Similar structure to HFS+ but simpler (smaller extent descriptors)

3. **(Optional) Extent overflow support** (4-6 hours)
   - Only needed for heavily fragmented files
   - Can defer until users report issues

---

## Test Files Needed

- HFS+ disc image with small files (single extent)
- HFS+ disc image with large files (multi-extent)
- HFS classic disc image
- Mixed mode CD with HFS partition (APM)

---

## Related Files

- `src/disc/browse/mod.rs` - Filesystem factory, APM detection
- `src/disc/browse/hfsplus_fs.rs` - HFS+ implementation
- `src/disc/browse/hfs_fs.rs` - HFS classic implementation
- `src/disc/browse/filesystem.rs` - Filesystem trait definition
- `src/disc/browse/reader.rs` - SectorReader trait and implementations
- `src/gui/browse_view.rs` - UI that calls read_file()

## References

- [HFS+ Specification (Apple TN1150)](https://developer.apple.com/library/archive/technotes/tn/tn1150.html)
- [HFS Format (low-level)](https://formats.kaitai.io/hfs_plus/)
- [Inside Macintosh: Files (HFS classic)](https://developer.apple.com/library/archive/documentation/mac/Files/Files-2.html)
