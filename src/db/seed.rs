//! First-run seed: unpack an ODE-lookup DB embedded at build time.
//!
//! Local builds never embed anything (`EMBED_LOOKUP_DB` is unset), so the
//! embedded byte slice is empty and `try_install_if_missing` is a no-op.
//! CI release builds embed the artifact downloaded by `build.rs`.

use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::paths::DbPaths;

/// Compressed seed DB bytes. Empty when `EMBED_LOOKUP_DB` was not set at
/// build time.
const SEED_ZST: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/ode-lookup.sqlite.zst"));

/// sha256 of `SEED_ZST` (coreutils format). Empty when no seed embedded.
const SEED_ZST_SHA256: &str =
    include_str!(concat!(env!("OUT_DIR"), "/ode-lookup.sqlite.zst.sha256"));

/// sha256 of the decompressed SQLite. Empty when no seed embedded.
const SEED_PLAIN_SHA256: &str =
    include_str!(concat!(env!("OUT_DIR"), "/ode-lookup.sqlite.sha256"));

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("seed is missing or malformed sha256")]
    BadSeedHash,
    #[error("decompressed seed sha256 mismatch (expected {expected}, got {got})")]
    HashMismatch { expected: String, got: String },
    #[error("zstd decode: {0}")]
    Zstd(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// No embedded seed (local build), nothing to do.
    NotEmbedded,
    /// Cache already has a DB; leave it alone.
    AlreadyInstalled,
    /// Seed was installed into the cache directory.
    Installed { bytes: u64 },
}

/// Install the embedded seed at `paths.sqlite()` if no DB exists yet.
/// Idempotent and safe to call on every startup.
pub fn try_install_if_missing(paths: &DbPaths) -> Result<SeedOutcome, SeedError> {
    if SEED_ZST.is_empty() {
        return Ok(SeedOutcome::NotEmbedded);
    }
    if paths.sqlite().exists() {
        return Ok(SeedOutcome::AlreadyInstalled);
    }

    let zst_hash = parse_sha256(SEED_ZST_SHA256).ok_or(SeedError::BadSeedHash)?;
    let plain_hash = parse_sha256(SEED_PLAIN_SHA256).ok_or(SeedError::BadSeedHash)?;

    // Decompress straight to disk so we don't hold 48 MB in memory.
    let tmp = paths.decompress_tmp();
    let _ = fs::remove_file(&tmp);
    let got_plain = decompress_to(&tmp)?;
    if !got_plain.eq_ignore_ascii_case(&plain_hash) {
        let _ = fs::remove_file(&tmp);
        return Err(SeedError::HashMismatch {
            expected: plain_hash,
            got: got_plain,
        });
    }

    let final_path = paths.sqlite();
    fs::rename(&tmp, &final_path)?;
    fs::write(paths.last_zst_sha256(), &zst_hash)?;

    let bytes = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
    Ok(SeedOutcome::Installed { bytes })
}

fn decompress_to(dest: &Path) -> Result<String, SeedError> {
    let mut decoder = zstd::stream::Decoder::with_buffer(SEED_ZST)
        .map_err(|e| SeedError::Zstd(e.to_string()))?;
    decoder
        .window_log_max(27)
        .map_err(|e| SeedError::Zstd(e.to_string()))?;

    let file = std::fs::File::create(dest)?;
    let mut writer = BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| SeedError::Zstd(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
    }
    writer.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

fn parse_sha256(body: &str) -> Option<String> {
    let first = body.trim_start().get(..64)?;
    if first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(first.to_ascii_lowercase())
    } else {
        None
    }
}
