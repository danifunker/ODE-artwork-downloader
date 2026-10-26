//! CD-DA audio for the in-app player: list a disc's audio tracks and stream a
//! track's PCM. Supports **CHD** and **BIN/CUE**; [`read_tracks`] and
//! [`extract_audio_pcm`] dispatch on the file's [`DiscFormat`].
//!
//! Both backings hand back the same little-endian, 2352-byte/sector CD-DA PCM:
//!
//! - **CHD**: [`libchdman_rs`]'s [`CdAudioReader`] (the copy we already link
//!   for hashing) decodes the requested track's sectors on demand, with the
//!   same bytes `extract_to_cue` / `chdman extractcd` would write: CHD's
//!   big-endian CD-DA swapped back to little-endian, 96-byte subcode dropped.
//!   It sits behind cd-da-reader's [`AudioSectorReader`] seam and is streamed
//!   ~1 s at a time, so there's no temp BIN and no whole-track buffer.
//! - **BIN/CUE**: the `.bin` is *already* little-endian 2352-B raw sectors laid
//!   out per the `.cue`, so there is nothing to decode. [`opticaldiscs::bincue`]
//!   parses the cue (track types, per-track BIN path + byte offset) and we read
//!   the audio track's sectors straight out of the `.bin` — including multi-BIN
//!   layouts, where each track names its own file.
//!
//! A CHD track's sectors start at the first sector the CHD stores for it,
//! which includes an `INDEX 00` pregap when the image keeps one, exactly as
//! the track's slice of an extracted `.bin` would.
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cd_da_reader::{open_track_stream_at, AudioSectorReader};
use libchdman_rs::cd::{list_tracks, CdAudioReader, TrackType};
use libchdman_rs::Chd;
use opticaldiscs::bincue::{self, BinTrack};
use opticaldiscs::formats::DiscFormat;

/// CD-DA sample rate (Hz).
pub const CDDA_SAMPLE_RATE: u32 = 44_100;
/// CD-DA channel count (stereo).
pub const CDDA_CHANNELS: u16 = 2;

/// Raw CD sector size: 2352 bytes = 588 interleaved 16-bit stereo sample-frames.
const BYTES_PER_SECTOR: usize = 2352;
/// One second of CD audio is 75 sectors.
const SECTORS_PER_SECOND: u32 = 75;

/// One CD track for the UI / example, from either a CHD or a BIN/CUE image.
#[derive(Debug, Clone)]
pub struct CdTrack {
    /// 1-based track number.
    pub number: u32,
    /// Track type label, e.g. `Audio`, `Mode1`.
    pub track_type: String,
    /// Track length in frames (sectors), excluding pregap.
    pub frames: u32,
    /// Pregap length in frames (0 when the backing doesn't expose it).
    pub pregap: u32,
    /// Whether this is a CD-DA (audio) track.
    pub is_audio: bool,
}

impl CdTrack {
    /// Track length formatted as `MM:SS` (75 frames/sec).
    pub fn duration_mmss(&self) -> String {
        let secs = self.frames / 75;
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// List a disc's CD tracks. Supports CHD and BIN/CUE; errors for other formats.
pub fn read_tracks(path: &Path) -> Result<Vec<CdTrack>, String> {
    match DiscFormat::from_path(path) {
        Some(DiscFormat::Chd) => read_tracks_chd(path),
        Some(DiscFormat::BinCue) => read_tracks_bincue(path),
        other => Err(format!("no CD-DA support for {other:?} ({})", path.display())),
    }
}

/// Stream an audio track's PCM, invoking `on_samples` with batches of
/// interleaved little-endian `i16` stereo samples, and return the total number
/// of `i16` samples emitted. Errors if the requested track isn't an audio track.
///
/// Dispatches on format: CHD extracts the disc once up front (the slow
/// "Preparing" cost) then slices; BIN/CUE streams straight from the `.bin`.
pub fn extract_audio_pcm<F>(path: &Path, track_number: u32, on_samples: F) -> Result<u64, String>
where
    F: FnMut(&[i16]),
{
    match DiscFormat::from_path(path) {
        Some(DiscFormat::Chd) => extract_audio_pcm_chd(path, track_number, on_samples),
        Some(DiscFormat::BinCue) => extract_audio_pcm_bincue(path, track_number, on_samples),
        other => Err(format!("no CD-DA support for {other:?} ({})", path.display())),
    }
}

/// Convert little-endian PCM bytes to interleaved `i16` samples, never relying
/// on host endianness.
fn pcm_le_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

// ── CHD backing ─────────────────────────────────────────────────────────────

/// Open a CHD CD image and return its track list.
fn read_tracks_chd(path: &Path) -> Result<Vec<CdTrack>, String> {
    let chd = open_chd(path)?;
    let tracks: Vec<CdTrack> = list_tracks(&chd)
        .map_err(|e| format!("list CHD tracks: {e:?}"))?
        .into_iter()
        .map(|t| CdTrack {
            number: t.track_num,
            track_type: format!("{:?}", t.track_type),
            frames: t.frames,
            pregap: t.pregap,
            is_audio: t.track_type == TrackType::Audio,
        })
        .collect();
    if tracks.is_empty() {
        return Err("no tracks in image".to_string());
    }
    Ok(tracks)
}

/// Stream a CHD audio track's PCM straight out of the CHD, ~1 s per callback.
/// Sectors are decoded on demand, so playback starts without unpacking the disc.
fn extract_audio_pcm_chd<F>(
    path: &Path,
    track_number: u32,
    mut on_samples: F,
) -> Result<u64, String>
where
    F: FnMut(&[i16]),
{
    let chd = open_chd(path)?;
    let tracks = list_tracks(&chd).map_err(|e| format!("list CHD tracks: {e:?}"))?;
    // CdAudioReader takes a 0-based index into this list, not the track number.
    let index = tracks
        .iter()
        .position(|t| t.track_num == track_number)
        .ok_or_else(|| format!("no track {track_number} on this disc"))?;
    // CdAudioReader also reads raw data tracks, so guard against playing a
    // data track's bytes as "audio".
    if tracks[index].track_type != TrackType::Audio {
        return Err(format!("track {track_number} is not an AUDIO track"));
    }

    // Per-track view: sector 0 is the track's first sector, so the bounds are
    // just 0..sector_count(). Unlike CdAudioReader::open, this doesn't reject
    // the whole disc when some other track is stored cooked (MODE1/2048).
    let reader = CdAudioReader::open_track(chd, index as u32)
        .map_err(|e| format!("open CHD track {track_number}: {e:?}"))?;
    let sectors = reader.sector_count();
    let src = ChdTrackAudio(Mutex::new(reader));
    let mut stream =
        open_track_stream_at(&src, 0, sectors).with_sectors_per_chunk(SECTORS_PER_SECOND);

    let mut total = 0u64;
    while let Some(bytes) = stream
        .next_chunk()
        .map_err(|e| format!("read track {track_number}: {e}"))?
    {
        let samples = pcm_le_i16(&bytes);
        total += samples.len() as u64;
        on_samples(&samples);
    }
    Ok(total)
}

/// One CHD track exposed to `cd-da-reader`. The trait reads through `&self`
/// but [`CdAudioReader::read_sectors`] needs `&mut self`, hence the `Mutex`.
struct ChdTrackAudio(Mutex<CdAudioReader>);

impl AudioSectorReader for ChdTrackAudio {
    type Error = std::io::Error;

    fn read_audio_sectors(&self, start_lba: u32, count: u32) -> Result<Vec<u8>, Self::Error> {
        let mut buf = vec![0u8; count as usize * CdAudioReader::SECTOR_SIZE];
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("CHD reader poisoned"))?
            .read_sectors(start_lba, count, &mut buf)
            .map_err(|e| std::io::Error::other(format!("read CHD sectors: {e:?}")))?;
        Ok(buf)
    }
}

/// Open a CHD read-only, mapping the path/CHD errors to a message.
fn open_chd(path: &Path) -> Result<Chd, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| format!("non-UTF-8 path: {}", path.display()))?;
    Chd::open(path_str, false, None).map_err(|e| format!("open CHD: {e:?}"))
}

// ── BIN/CUE backing ───────────────────────────────────────────────────────────

/// Parse a `.cue` (or the `.cue` sibling of a `.bin`) into a track list.
fn read_tracks_bincue(path: &Path) -> Result<Vec<CdTrack>, String> {
    let cue = resolve_cue(path)?;
    let tracks = bincue::parse_cue_tracks(&cue).map_err(|e| format!("parse cue: {e}"))?;
    let mut out = Vec::with_capacity(tracks.len());
    for t in &tracks {
        out.push(CdTrack {
            number: t.track_no,
            track_type: format!("{:?}", t.track_type),
            frames: bincue_frames(t)?,
            pregap: 0,
            is_audio: t.track_type == bincue::TrackType::Audio,
        });
    }
    if out.is_empty() {
        return Err("no tracks in cue".to_string());
    }
    Ok(out)
}

/// Stream a BIN/CUE audio track's PCM straight from the `.bin`. Audio sectors are
/// already little-endian 2352-B raw PCM, so no decode — just seek and read.
fn extract_audio_pcm_bincue<F>(
    path: &Path,
    track_number: u32,
    mut on_samples: F,
) -> Result<u64, String>
where
    F: FnMut(&[i16]),
{
    let cue = resolve_cue(path)?;
    let tracks = bincue::parse_cue_tracks(&cue).map_err(|e| format!("parse cue: {e}"))?;
    let track = tracks
        .iter()
        .find(|t| t.track_no == track_number)
        .ok_or_else(|| format!("no track {track_number} on this disc"))?;
    if track.track_type != bincue::TrackType::Audio {
        return Err(format!("track {track_number} is not an AUDIO track"));
    }
    let frames = bincue_frames(track)?;

    let mut f = std::fs::File::open(&track.bin_path)
        .map_err(|e| format!("open {}: {e}", track.bin_path.display()))?;
    f.seek(SeekFrom::Start(track.file_byte_offset))
        .map_err(|e| format!("seek {}: {e}", track.bin_path.display()))?;

    // Emit ~1 s (75 sectors) per callback so the player starts quickly and never
    // holds the whole track in memory.
    let mut buf = vec![0u8; SECTORS_PER_SECOND as usize * BYTES_PER_SECTOR];
    let mut remaining = frames;
    let mut total = 0u64;
    while remaining > 0 {
        let n = remaining.min(SECTORS_PER_SECOND);
        let bytes = n as usize * BYTES_PER_SECTOR;
        f.read_exact(&mut buf[..bytes])
            .map_err(|e| format!("read track {track_number}: {e}"))?;
        let samples = pcm_le_i16(&buf[..bytes]);
        total += samples.len() as u64;
        on_samples(&samples);
        remaining -= n;
    }
    Ok(total)
}

/// Frames (sectors) in a BIN/CUE track. opticaldiscs derives this from adjacent
/// INDEX 01 positions, but returns 0 for the last track in a BIN file (nothing
/// after it to measure against); fall back to the BIN file's size there.
fn bincue_frames(t: &BinTrack) -> Result<u32, String> {
    if t.frame_count > 0 {
        return Ok(t.frame_count as u32);
    }
    let len = std::fs::metadata(&t.bin_path)
        .map_err(|e| format!("stat {}: {e}", t.bin_path.display()))?
        .len();
    let avail = len.saturating_sub(t.file_byte_offset);
    Ok((avail / t.sector_size()) as u32)
}

/// Resolve a BIN/CUE path to its `.cue`: pass a `.cue` through, or find the
/// `.cue` sitting next to a `.bin`.
fn resolve_cue(path: &Path) -> Result<PathBuf, String> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("cue") => Ok(path.to_path_buf()),
        Some("bin") => {
            let cue = path.with_extension("cue");
            if cue.exists() {
                Ok(cue)
            } else {
                Err(format!("no .cue sheet found next to {}", path.display()))
            }
        }
        _ => Err(format!("not a BIN/CUE path: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_mmss() {
        let t = CdTrack {
            number: 1,
            track_type: "Audio".to_string(),
            frames: 75 * 90, // 90 seconds
            pregap: 0,
            is_audio: true,
        };
        assert_eq!(t.duration_mmss(), "01:30");
    }
}
