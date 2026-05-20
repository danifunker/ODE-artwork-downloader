//! Track-level hashing for redump lookup.
//!
//! Redump stores hashes of the raw 2352-byte per-sector BIN data, so for
//! BIN/CUE we open the BIN directly and feed the raw track bytes through
//! all three hashers in a single streaming pass. Plain ISO files are hashed
//! end-to-end — that doesn't match the typical BIN-based redump entries but
//! catches the minority of redump entries stored as raw .iso. CHD is not
//! yet supported (it needs frame-by-frame subcode stripping; planned).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crc32fast::Hasher as Crc32;
use libchdman_rs::cd::{extract_to_cue, list_tracks};
use libchdman_rs::Chd;
use md5::{Digest as Md5Digest, Md5};
use opticaldiscs::bincue::parse_cue_tracks;
use opticaldiscs::formats::DiscFormat;
use sha1::Sha1;
use thiserror::Error;

use super::reader::DiscInfo;

/// Read buffer size for streaming hash. 1 MiB balances syscall overhead
/// against responsiveness of the progress counter.
const READ_BUF: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct TrackHashes {
    pub sha1: String,
    pub md5: String,
    pub crc32: String,
    pub size_bytes: u64,
    /// What was hashed, in human terms (e.g. "BIN track 1", "ISO file").
    /// Useful for log lines.
    pub source: String,
}

/// Progress + cancellation channel, written by the worker thread and read
/// by the UI loop. Stays on the heap inside an `Arc<Mutex<…>>`.
#[derive(Debug, Default)]
pub struct HashProgress {
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub stage: String,
    pub active: bool,
    pub cancelled: bool,
}

impl HashProgress {
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.current_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
        }
    }
}

#[derive(Debug, Error)]
pub enum HashError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("opticaldiscs: {0}")]
    Opticaldiscs(String),
    #[error("no data track found")]
    NoDataTrack,
    #[error("format not yet supported for hashing: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
}

impl From<opticaldiscs::error::OpticaldiscsError> for HashError {
    fn from(e: opticaldiscs::error::OpticaldiscsError) -> Self {
        HashError::Opticaldiscs(e.to_string())
    }
}

/// Compute SHA1 / MD5 / CRC32 of the first data track in `info` (or the
/// whole file for plain ISOs). Calls into `progress` periodically; bails
/// out with `Cancelled` if `progress.cancelled` flips true mid-flight.
pub fn hash_data_track(
    info: &DiscInfo,
    progress: Arc<Mutex<HashProgress>>,
) -> Result<TrackHashes, HashError> {
    match info.format {
        DiscFormat::BinCue => hash_bincue(&info.path, &progress),
        DiscFormat::Iso => hash_iso(&info.path, &progress),
        DiscFormat::Chd => hash_chd(&info.path, &progress),
        // MDS/MDF (and anything else) not implemented yet.
        other => Err(HashError::Unsupported(format!("{other:?}"))),
    }
}

fn hash_bincue(
    cue_or_bin: &Path,
    progress: &Arc<Mutex<HashProgress>>,
) -> Result<TrackHashes, HashError> {
    // Accept either a .cue path or a .bin path with a sibling .cue.
    let cue_path = if cue_or_bin
        .extension()
        .map(|e| e.eq_ignore_ascii_case("cue"))
        .unwrap_or(false)
    {
        cue_or_bin.to_path_buf()
    } else {
        cue_or_bin.with_extension("cue")
    };

    if !cue_path.exists() {
        return Err(HashError::Opticaldiscs(format!(
            "missing cue sheet next to {}",
            cue_or_bin.display()
        )));
    }

    let tracks = parse_cue_tracks(&cue_path)?;
    let track = tracks
        .iter()
        .find(|t| t.is_data())
        .ok_or(HashError::NoDataTrack)?;

    // Bytes to read: prefer the cue-declared frame_count when available;
    // otherwise use the gap to the next track (or file end) as a fallback.
    let total = if track.frame_count > 0 {
        track.frame_count * track.sector_size()
    } else {
        let next_offset = tracks
            .iter()
            .filter(|t| t.bin_path == track.bin_path && t.file_byte_offset > track.file_byte_offset)
            .map(|t| t.file_byte_offset)
            .min();
        match next_offset {
            Some(end) => end.saturating_sub(track.file_byte_offset),
            None => {
                let len = std::fs::metadata(&track.bin_path)?.len();
                len.saturating_sub(track.file_byte_offset)
            }
        }
    };

    let mut file = BufReader::new(File::open(&track.bin_path)?);
    file.seek(SeekFrom::Start(track.file_byte_offset))?;

    let source = format!("BIN track {} (raw)", track.track_no);
    {
        let mut p = progress.lock().unwrap();
        p.stage = format!("Hashing {source}");
        p.total_bytes = total;
        p.current_bytes = 0;
        p.active = true;
    }

    let hashed = stream_hash(&mut file, total, progress)?;
    Ok(TrackHashes {
        sha1: hashed.sha1,
        md5: hashed.md5,
        crc32: hashed.crc32,
        size_bytes: hashed.size_bytes,
        source,
    })
}

fn hash_iso(
    path: &Path,
    progress: &Arc<Mutex<HashProgress>>,
) -> Result<TrackHashes, HashError> {
    let total = std::fs::metadata(path)?.len();
    let mut file = BufReader::new(File::open(path)?);

    let source = "ISO file".to_string();
    {
        let mut p = progress.lock().unwrap();
        p.stage = format!("Hashing {source}");
        p.total_bytes = total;
        p.current_bytes = 0;
        p.active = true;
    }

    let hashed = stream_hash(&mut file, total, progress)?;
    Ok(TrackHashes {
        sha1: hashed.sha1,
        md5: hashed.md5,
        crc32: hashed.crc32,
        size_bytes: hashed.size_bytes,
        source,
    })
}

/// CHD path: extract to a temp BIN/CUE using libchdman-rs (which wraps
/// MAME's `chdman` core, so the BIN is byte-identical to redump's source),
/// then hash track 1 of the BIN with the existing BIN/CUE path.
///
/// Two stages get progress: "Extracting CHD" (libchdman-rs decompression)
/// and "Hashing BIN track 1 (raw)" (handed off to `hash_bincue`). The
/// `RateTracker` resets its rolling window on stage change so the ETA stays
/// sane across the transition.
fn hash_chd(
    chd_path: &Path,
    progress: &Arc<Mutex<HashProgress>>,
) -> Result<TrackHashes, HashError> {
    // Open the CHD just to enumerate tracks so we can guess a sensible
    // total-bytes target for the extract progress bar.
    let chd_path_str = chd_path
        .to_str()
        .ok_or_else(|| HashError::Opticaldiscs(format!("non-UTF8 path: {}", chd_path.display())))?;
    let chd = Chd::open(chd_path_str, false, None)
        .map_err(|e| HashError::Opticaldiscs(format!("open CHD: {e:?}")))?;
    let tracks = list_tracks(&chd)
        .map_err(|e| HashError::Opticaldiscs(format!("list CHD tracks: {e:?}")))?;
    // Conservative upper bound: every frame at the raw 2352 sector size.
    // Cooked Mode1 tracks would write 2048/frame so the counter tops out
    // a bit short — acceptable for a progress estimate.
    let extract_total: u64 = tracks
        .iter()
        .map(|t| t.frames as u64 * 2352)
        .sum();
    drop(chd);

    let tmp = tempfile::tempdir()?;
    let cue_path = tmp.path().join("disc.cue");
    let bin_path = tmp.path().join("disc.bin");

    {
        let mut p = progress.lock().unwrap();
        p.stage = "Extracting CHD".into();
        p.total_bytes = extract_total;
        p.current_bytes = 0;
        p.active = true;
        p.cancelled = false;
    }

    // libchdman-rs's extract callback fires per frame with the cumulative
    // bytes-written counter. Forward straight into HashProgress.
    let progress_for_cb = Arc::clone(progress);
    let mut cb = move |bytes_written: u64| {
        if let Ok(mut p) = progress_for_cb.lock() {
            p.current_bytes = bytes_written;
        }
    };

    extract_to_cue(chd_path, &cue_path, &bin_path, &mut cb)
        .map_err(|e| HashError::Opticaldiscs(format!("CHD extract: {e:?}")))?;

    // Cancellation check between stages.
    if progress.lock().unwrap().cancelled {
        return Err(HashError::Cancelled);
    }

    // Hand the extracted BIN/CUE to the regular hashing path. `hash_bincue`
    // overwrites `stage`/`total_bytes`/`current_bytes`, so `RateTracker`
    // will see a new stage label and reset its rolling window.
    let mut hashes = hash_bincue(&cue_path, progress)?;

    // Re-label so the success log says "from a CHD" rather than "from a
    // BIN" (which would be confusing — the user picked a .chd).
    hashes.source = format!("CHD track 1 (raw, extracted)");

    // `tmp` drops here, cleaning up the extracted BIN/CUE.
    Ok(hashes)
}

struct StreamHashResult {
    sha1: String,
    md5: String,
    crc32: String,
    size_bytes: u64,
}

fn stream_hash<R: Read>(
    reader: &mut R,
    bytes_to_read: u64,
    progress: &Arc<Mutex<HashProgress>>,
) -> Result<StreamHashResult, HashError> {
    let mut sha1 = Sha1::new();
    let mut md5 = Md5::new();
    let mut crc32 = Crc32::new();

    let mut buf = vec![0u8; READ_BUF];
    let mut remaining = bytes_to_read;
    let mut hashed: u64 = 0;
    // Throttle mutex traffic: only push a progress update every ~16 MiB or
    // ~250 ms of work, whichever comes first.
    let mut last_update = std::time::Instant::now();
    let mut bytes_since_update: u64 = 0;

    while remaining > 0 {
        if progress.lock().unwrap().cancelled {
            return Err(HashError::Cancelled);
        }

        let want = (buf.len() as u64).min(remaining) as usize;
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            // Short read — happens when the cue lies about frame_count.
            break;
        }
        sha1.update(&buf[..n]);
        md5.update(&buf[..n]);
        crc32.update(&buf[..n]);
        hashed += n as u64;
        remaining = remaining.saturating_sub(n as u64);
        bytes_since_update += n as u64;

        if bytes_since_update >= 16 * 1024 * 1024
            || last_update.elapsed() >= std::time::Duration::from_millis(250)
        {
            let mut p = progress.lock().unwrap();
            p.current_bytes = hashed;
            last_update = std::time::Instant::now();
            bytes_since_update = 0;
        }
    }

    {
        let mut p = progress.lock().unwrap();
        p.current_bytes = hashed;
        p.active = false;
    }

    Ok(StreamHashResult {
        sha1: hex::encode(sha1.finalize()),
        md5: hex::encode(md5.finalize()),
        crc32: format!("{:08x}", crc32.finalize()),
        size_bytes: hashed,
    })
}
