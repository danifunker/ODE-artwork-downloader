//! Platform-appropriate paths for the cached redump DB.

use std::path::PathBuf;

use directories::ProjectDirs;

pub struct DbPaths {
    pub data_dir: PathBuf,
}

impl DbPaths {
    pub fn discover() -> Result<Self, String> {
        let dirs = ProjectDirs::from("", "", "ODE-artwork-downloader")
            .ok_or_else(|| "could not resolve a per-user data directory".to_string())?;
        let data_dir = dirs.data_dir().to_path_buf();
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;
        Ok(Self { data_dir })
    }

    pub fn sqlite(&self) -> PathBuf {
        self.data_dir.join("redump.sqlite")
    }

    /// Cached sha256 of the most recently installed `.zst` artifact. Stored as
    /// the raw 64-char hex string. Used to short-circuit downloads when the
    /// upstream artifact hasn't changed.
    pub fn last_zst_sha256(&self) -> PathBuf {
        self.data_dir.join("redump.sqlite.zst.sha256")
    }

    /// Temp location for the in-flight download. Sits next to the live DB so
    /// the final atomic rename stays on the same filesystem.
    pub fn download_tmp(&self) -> PathBuf {
        self.data_dir.join("redump.sqlite.zst.partial")
    }

    /// Temp location for the decompressed DB before the swap.
    pub fn decompress_tmp(&self) -> PathBuf {
        self.data_dir.join("redump.sqlite.partial")
    }
}
