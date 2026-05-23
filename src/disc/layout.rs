//! Sector-layout detection.
//!
//! Redump dumps CD data tracks as **raw 2352-byte sectors** (full sector with
//! sync/header/ECC/EDC), so its track hashes are computed over that layout. A
//! "cooked" 2048-byte-sector ISO contains only user data, so its hash can
//! never equal a redump track hash — hashing it for a redump lookup is wasted
//! work.
//!
//! This module classifies an image cheaply (one small read) so callers can
//! skip hashing when it cannot possibly match.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// 12-byte sync pattern that begins every raw (2352-byte) CD sector.
const SYNC: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

const RAW_SECTOR: u64 = 2352;
const COOKED_SECTOR: u64 = 2048;
/// ISO9660 PVD lives at logical sector 16; in a cooked image that's this byte
/// offset, where the descriptor's `CD001` magic (at +1) should appear.
const COOKED_PVD_OFFSET: u64 = 16 * COOKED_SECTOR;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectorLayout {
    /// Raw 2352-byte sectors — hashes can match redump.
    Raw2352,
    /// Cooked 2048-byte sectors — hashes cannot match redump.
    Cooked2048,
    /// Could not determine (unreadable, or neither signature found).
    Unknown,
}

impl SectorLayout {
    /// Whether hashing this layout could match a redump raw-track hash.
    pub fn is_hashable_for_redump(self) -> bool {
        matches!(self, SectorLayout::Raw2352)
    }
}

/// Classify a disc-image file by reading only its header (and the cooked-PVD
/// offset). Does not scan the whole file.
pub fn detect_sector_layout(path: &Path) -> SectorLayout {
    let Ok(mut f) = File::open(path) else {
        return SectorLayout::Unknown;
    };

    // Raw images begin with the sync pattern.
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_ok() && head == SYNC {
        return SectorLayout::Raw2352;
    }

    // Cooked ISO: `CD001` at the logical-sector-16 offset.
    if let Ok(magic) = read_at(&mut f, COOKED_PVD_OFFSET + 1, 5) {
        if magic == b"CD001" {
            return SectorLayout::Cooked2048;
        }
    }

    // Fall back to file-size divisibility as a weak corroborator.
    if let Ok(meta) = path.metadata() {
        let len = meta.len();
        match (len % RAW_SECTOR == 0, len % COOKED_SECTOR == 0) {
            (true, false) => return SectorLayout::Raw2352,
            (false, true) => return SectorLayout::Cooked2048,
            _ => {}
        }
    }

    SectorLayout::Unknown
}

fn read_at(f: &mut File, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn detects_raw_by_sync() {
        let mut buf = SYNC.to_vec();
        buf.extend_from_slice(&[0u8; 100]);
        let f = write_temp(&buf);
        assert_eq!(detect_sector_layout(f.path()), SectorLayout::Raw2352);
    }

    #[test]
    fn detects_cooked_by_pvd_magic() {
        let mut buf = vec![0u8; COOKED_PVD_OFFSET as usize + 8];
        buf[COOKED_PVD_OFFSET as usize] = 0x01; // descriptor type
        buf[COOKED_PVD_OFFSET as usize + 1..COOKED_PVD_OFFSET as usize + 6]
            .copy_from_slice(b"CD001");
        let f = write_temp(&buf);
        assert_eq!(detect_sector_layout(f.path()), SectorLayout::Cooked2048);
    }

    #[test]
    fn unknown_when_no_signature() {
        let f = write_temp(&[0x12, 0x34, 0x56, 0x78, 0x9a]);
        assert_eq!(detect_sector_layout(f.path()), SectorLayout::Unknown);
    }

    #[test]
    fn hashable_only_for_raw() {
        assert!(SectorLayout::Raw2352.is_hashable_for_redump());
        assert!(!SectorLayout::Cooked2048.is_hashable_for_redump());
        assert!(!SectorLayout::Unknown.is_hashable_for_redump());
    }
}
