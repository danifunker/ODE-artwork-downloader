//! HTTP, hashing, and zstd decompression for the ODE-lookup DB artifact.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{LATEST_RELEASE_BASE, USER_AGENT};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed .sha256 file from {url}")]
    BadHashFile { url: String },
    #[error("compressed sha256 mismatch (expected {expected}, got {got})")]
    CompressedHashMismatch { expected: String, got: String },
    #[error("decompressed sha256 mismatch (expected {expected}, got {got})")]
    DecompressedHashMismatch { expected: String, got: String },
    #[error("zstd decode: {0}")]
    Zstd(String),
}

pub struct Urls {
    pub zst: String,
    pub zst_sha256: String,
    pub plain_sha256: String,
}

impl Urls {
    pub fn latest() -> Self {
        Self {
            zst: format!("{LATEST_RELEASE_BASE}/ode-lookup.sqlite.zst"),
            zst_sha256: format!("{LATEST_RELEASE_BASE}/ode-lookup.sqlite.zst.sha256"),
            plain_sha256: format!("{LATEST_RELEASE_BASE}/ode-lookup.sqlite.sha256"),
        }
    }
}

pub fn build_client() -> Result<reqwest::blocking::Client, FetchError> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(5 * 60))
        .build()?)
}

/// Fetch a `.sha256` file and parse out the hex digest. coreutils format is
/// `<hex>  <filename>\n`, but we accept anything that starts with 64 hex chars.
pub fn fetch_sha256(client: &reqwest::blocking::Client, url: &str) -> Result<String, FetchError> {
    let body = client.get(url).send()?.error_for_status()?.text()?;
    parse_sha256(&body).ok_or_else(|| FetchError::BadHashFile {
        url: url.to_string(),
    })
}

fn parse_sha256(body: &str) -> Option<String> {
    let first = body.trim_start().get(..64)?;
    if first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(first.to_ascii_lowercase())
    } else {
        None
    }
}

/// Download `url` to `dest`, computing sha256 as we go. Returns the hex digest.
pub fn download_with_hash(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
) -> Result<String, FetchError> {
    let mut response = client.get(url).send()?.error_for_status()?;
    let file = File::create(dest)?;
    let mut writer = BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
    }
    writer.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

/// Decompress `src` (zstd) into `dest`, computing sha256 of the plaintext.
/// Streams; does not hold the full payload in memory.
pub fn decompress_with_hash(src: &Path, dest: &Path) -> Result<String, FetchError> {
    let input = File::open(src)?;
    let mut decoder =
        zstd::stream::Decoder::new(input).map_err(|e| FetchError::Zstd(e.to_string()))?;
    // The release uses --long=27. Default cap in libzstd >= 1.4 is already 27,
    // but set it explicitly so older zstd builds don't refuse the window.
    decoder
        .window_log_max(27)
        .map_err(|e| FetchError::Zstd(e.to_string()))?;

    let file = File::create(dest)?;
    let mut writer = BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| FetchError::Zstd(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
    }
    writer.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

/// Verify two sha256 hex strings match (case-insensitive). Returns the error
/// flavor for the compressed file.
pub fn check_compressed(expected: &str, got: &str) -> Result<(), FetchError> {
    if expected.eq_ignore_ascii_case(got) {
        Ok(())
    } else {
        Err(FetchError::CompressedHashMismatch {
            expected: expected.to_ascii_lowercase(),
            got: got.to_ascii_lowercase(),
        })
    }
}

pub fn check_decompressed(expected: &str, got: &str) -> Result<(), FetchError> {
    if expected.eq_ignore_ascii_case(got) {
        Ok(())
    } else {
        Err(FetchError::DecompressedHashMismatch {
            expected: expected.to_ascii_lowercase(),
            got: got.to_ascii_lowercase(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_coreutils_format() {
        let body = "abc1230000000000000000000000000000000000000000000000000000000000  ode-lookup.sqlite.zst\n";
        let got = parse_sha256(body).unwrap();
        assert_eq!(got.len(), 64);
        assert!(got.starts_with("abc123"));
    }

    #[test]
    fn parse_bare_hex() {
        let body = "ABC1230000000000000000000000000000000000000000000000000000000000\n";
        let got = parse_sha256(body).unwrap();
        assert_eq!(got, "abc1230000000000000000000000000000000000000000000000000000000000");
    }

    #[test]
    fn parse_rejects_short() {
        assert!(parse_sha256("deadbeef\n").is_none());
    }
}
