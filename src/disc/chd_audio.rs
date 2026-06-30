//! CD-DA audio extraction, delegated to the `rust-cdda` library.
//!
//! This is a thin adapter over [`rust_cdda`] so the in-app player and the
//! `play_chd` example can list tracks and stream PCM without depending on
//! rust-cdda's exact types. rust-cdda owns the real work — opening CHD / BIN+CUE
//! images, the big-endian → little-endian CD-DA swap, and the 2448 → 2352
//! subcode strip. Playing a track back here is precisely how we verify that
//! pipeline produces correct PCM.

use std::path::Path;

use rust_cdda::CdImage;

/// CD-DA sample rate (Hz).
pub const CDDA_SAMPLE_RATE: u32 = 44_100;
/// CD-DA channel count (stereo).
pub const CDDA_CHANNELS: u16 = 2;

/// One CD track, adapted from [`rust_cdda::Track`] for the UI/example.
#[derive(Debug, Clone)]
pub struct ChdCdTrack {
    /// 1-based track number.
    pub number: u32,
    /// Track type label, e.g. `Audio`, `Mode1`.
    pub track_type: String,
    /// Track length in frames (sectors), excluding pregap.
    pub frames: u32,
    /// Pregap length in frames.
    pub pregap: u32,
    /// Whether this is a CD-DA (audio) track.
    pub is_audio: bool,
}

impl ChdCdTrack {
    /// Track length formatted as `MM:SS` (75 frames/sec).
    pub fn duration_mmss(&self) -> String {
        let secs = self.frames / 75;
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// Open a CD image (CHD or BIN/CUE) and return its track list.
pub fn read_tracks(path: &Path) -> Result<Vec<ChdCdTrack>, String> {
    let img = CdImage::open(path).map_err(|e| format!("open image: {e}"))?;
    let tracks: Vec<ChdCdTrack> = img
        .tracks()
        .iter()
        .map(|t| ChdCdTrack {
            number: t.number,
            track_type: t.kind.to_string(),
            frames: t.length_frames,
            pregap: t.pregap_frames,
            is_audio: t.kind.is_audio(),
        })
        .collect();
    if tracks.is_empty() {
        return Err("no tracks in image".to_string());
    }
    Ok(tracks)
}

/// Stream an audio track's PCM, invoking `on_samples` with batches of
/// interleaved little-endian `i16` stereo samples, and return the total number
/// of `i16` samples emitted.
///
/// rust-cdda's [`AudioReader`](rust_cdda::AudioReader) is pull-based and streams
/// hunk-by-hunk, so memory stays flat regardless of track length. Errors if the
/// requested track isn't an audio track.
pub fn extract_audio_pcm<F>(
    path: &Path,
    track_number: u32,
    mut on_samples: F,
) -> Result<u64, String>
where
    F: FnMut(&[i16]),
{
    let img = CdImage::open(path).map_err(|e| format!("open image: {e}"))?;
    let mut reader = img
        .audio_reader(track_number)
        .map_err(|e| format!("open audio track {track_number}: {e}"))?;

    // One CD frame = 588 stereo frames = 1176 i16; batch ~1 second per read to
    // keep per-call overhead low.
    let mut buf = vec![0i16; 1176 * 75];
    let mut total = 0u64;
    loop {
        let n = reader
            .read_samples(&mut buf)
            .map_err(|e| format!("read audio: {e}"))?;
        if n == 0 {
            break;
        }
        on_samples(&buf[..n]);
        total += n as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_mmss() {
        let t = ChdCdTrack {
            number: 1,
            track_type: "Audio".to_string(),
            frames: 75 * 90, // 90 seconds
            pregap: 0,
            is_audio: true,
        };
        assert_eq!(t.duration_mmss(), "01:30");
    }
}
