//! CD-DA audio extraction: ODE decodes CHD, `cd-da-reader` supplies the TOC math.
//!
//! [`cd_da_reader`] is deliberately format-agnostic — it bakes in no image
//! format. Instead it defines the [`AudioSectorReader`] seam (2352-byte, 16-bit
//! signed little-endian stereo sectors) plus its `Toc`/`Track` types. We own the
//! other half: decoding a MAME-style `.chd` to raw CD-DA sectors via the
//! [`libchdman_rs`] we already link for hashing.
//!
//! We resolve track bounds ourselves from the cumulative TOC rather than via
//! cd-da-reader's `read_track`: our extracted BIN is gapless, so cd-da-reader's
//! CD-Extra trailing-gap subtraction (correct for a physical disc) would drop
//! real audio here. See [`extract_audio_pcm`].
//!
//! Concretely, [`ChdDisc`] extracts the whole disc to a temporary redump-style
//! BIN with [`extract_to_cue`] — which already byte-swaps CHD's big-endian
//! CD-DA back to little-endian and strips the 96-byte subcode — then slices that
//! BIN per sector. Playing a track back through this path is precisely how we
//! verify the PCM is correct (right pitch/speed, no static), matching a
//! `chdman extractcd` rip byte-for-byte.

use std::path::{Path, PathBuf};

use cd_da_reader::{lba_to_msf, AudioSectorReader, Toc, Track};
use libchdman_rs::cd::{extract_to_cue, list_tracks, TrackType};
use libchdman_rs::Chd;

/// CD-DA sample rate (Hz).
pub const CDDA_SAMPLE_RATE: u32 = 44_100;
/// CD-DA channel count (stereo).
pub const CDDA_CHANNELS: u16 = 2;

/// Raw CD sector size: 2352 bytes = 588 interleaved 16-bit stereo sample-frames.
const BYTES_PER_SECTOR: usize = 2352;

/// One CD track, adapted from [`libchdman_rs::cd::TrackInfo`] for the UI/example.
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

/// Open a CHD CD image and return its track list.
pub fn read_tracks(path: &Path) -> Result<Vec<ChdCdTrack>, String> {
    let chd = open_chd(path)?;
    let tracks: Vec<ChdCdTrack> = list_tracks(&chd)
        .map_err(|e| format!("list CHD tracks: {e:?}"))?
        .into_iter()
        .map(|t| ChdCdTrack {
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

/// Stream an audio track's PCM, invoking `on_samples` with batches of
/// interleaved little-endian `i16` stereo samples, and return the total number
/// of `i16` samples emitted. Errors if the requested track isn't an audio track.
///
/// A whole audio track is bounded (~30 MB for 3 min), so this reads the track in
/// one [`AudioSectorReader::read_audio_sectors`] call and then emits it in
/// ~1-second batches to preserve the streaming callback contract. (For the old
/// low-latency start / flat memory, loop that call over the track's sector range
/// instead — same bytes, streamed; noted as a follow-up in the design doc.)
pub fn extract_audio_pcm<F>(
    path: &Path,
    track_number: u32,
    mut on_samples: F,
) -> Result<u64, String>
where
    F: FnMut(&[i16]),
{
    // ChdDisc::open is the slow "Preparing" cost (whole-disc extract); the
    // per-track read below is fast.
    let disc = ChdDisc::open(path)?;

    // Guard against reading a data track's bytes as "audio".
    let idx = disc
        .toc
        .tracks
        .iter()
        .position(|t| u32::from(t.number) == track_number)
        .ok_or_else(|| format!("no track {track_number} on this disc"))?;
    let track = &disc.toc.tracks[idx];
    if !track.is_audio {
        return Err(format!("track {track_number} is not an AUDIO track"));
    }

    // Bounds come straight from our cumulative TOC: the extracted BIN is gapless
    // (each track is exactly `frames` sectors, back-to-back), so a track spans
    // from its own `start_lba` to the next track's (or the leadout).
    //
    // We deliberately do NOT use cd-da-reader's `read_track` here. Its
    // `get_track_bounds` applies a CD-Extra rule that subtracts a fixed
    // 11,400-sector inter-session gap from the last audio track before a data
    // track. That gap is real on a physical disc but is absent from a
    // CHD-derived image, so honouring it would chop ~2.5 min of real audio off
    // that track. Every other track reads identically to `read_track`.
    let start_lba = track.start_lba;
    let end_lba = disc
        .toc
        .tracks
        .get(idx + 1)
        .map(|t| t.start_lba)
        .unwrap_or(disc.toc.leadout_lba);
    if end_lba <= start_lba {
        return Err(format!("track {track_number}: bad TOC bounds"));
    }
    let pcm = disc
        .read_audio_sectors(start_lba, end_lba - start_lba)
        .map_err(|e| format!("read track {track_number}: {e}"))?;

    // Batch ~1 second of audio per callback: 1176 i16 per CD frame × 75 frames/s.
    // Convert bytes → i16 with from_le_bytes so we never rely on host endianness.
    const BYTES_PER_BATCH: usize = 1176 * 75 * 2;
    let mut total = 0u64;
    for chunk in pcm.chunks(BYTES_PER_BATCH) {
        let samples: Vec<i16> = chunk
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        total += samples.len() as u64;
        on_samples(&samples);
    }
    Ok(total)
}

/// A CHD CD image extracted to a temporary redump-style BIN, exposing its audio
/// sectors to `cd-da-reader` via [`AudioSectorReader`].
struct ChdDisc {
    /// Temp BIN written by [`extract_to_cue`]: little-endian, 2352 B/sector,
    /// tracks laid out back-to-back.
    bin_path: PathBuf,
    /// TOC describing where each track's sectors live in `bin_path`.
    toc: Toc,
    /// Owns the temp dir; dropping it deletes the extracted BIN/CUE.
    _tmp: tempfile::TempDir,
}

impl ChdDisc {
    /// Extract `chd` to a temp BIN and build the matching `cd-da-reader` TOC.
    fn open(chd: &Path) -> Result<Self, String> {
        let handle = open_chd(chd)?;
        let tracks = list_tracks(&handle).map_err(|e| format!("list CHD tracks: {e:?}"))?;
        if tracks.is_empty() {
            return Err("no tracks in image".to_string());
        }

        // Extract the whole disc to a temp BIN. extract_to_cue already yields
        // little-endian 2352-B sectors with subcode stripped, so ChdDisc only
        // has to slice bytes.
        let tmp = tempfile::Builder::new()
            .prefix("ode-chd-audio-")
            .tempdir()
            .map_err(|e| format!("create temp dir: {e}"))?;
        let bin_path = tmp.path().join("disc.bin");
        let cue_path = tmp.path().join("disc.cue");
        let mut on_progress = |_written: u64| {};
        extract_to_cue(chd, &cue_path, &bin_path, &mut on_progress)
            .map_err(|e| format!("extract CHD to BIN: {e:?}"))?;

        // Build cd-da-reader's TOC from cumulative stored-frame offsets, which is
        // exactly extract_to_cue's single-BIN layout: each track occupies
        // `frames` sectors of 2352 bytes, back-to-back, so a track's absolute
        // sector index (start_lba) doubles as its BIN byte offset ÷ 2352.
        //
        // NB: this assumes the common raw-2352 CHD (no chdman pad/split frames
        // and no cooked data tracks). If a disc ever sounds wrong, diff a dumped
        // WAV against `chdman extractcd` and reconcile against the `.cue` that
        // extract_to_cue just wrote.
        let mut cd_tracks = Vec::with_capacity(tracks.len());
        let mut lba = 0u32;
        for t in &tracks {
            cd_tracks.push(Track {
                number: t.track_num as u8, // CDs have <= 99 tracks
                start_lba: lba,
                start_msf: lba_to_msf(lba),
                is_audio: t.track_type == TrackType::Audio,
            });
            lba = lba.saturating_add(t.frames);
        }
        let first_track = tracks.iter().map(|t| t.track_num as u8).min().unwrap_or(1);
        let last_track = tracks.iter().map(|t| t.track_num as u8).max().unwrap_or(1);
        let toc = Toc {
            first_track,
            last_track,
            tracks: cd_tracks,
            leadout_lba: lba,
        };

        Ok(Self {
            bin_path,
            toc,
            _tmp: tmp,
        })
    }
}

impl AudioSectorReader for ChdDisc {
    type Error = std::io::Error;

    fn read_audio_sectors(&self, start_lba: u32, count: u32) -> Result<Vec<u8>, Self::Error> {
        use std::io::{Read, Seek, SeekFrom};

        let mut f = std::fs::File::open(&self.bin_path)?;
        f.seek(SeekFrom::Start(
            u64::from(start_lba) * BYTES_PER_SECTOR as u64,
        ))?;
        let mut buf = vec![0u8; count as usize * BYTES_PER_SECTOR];
        f.read_exact(&mut buf)?;
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
