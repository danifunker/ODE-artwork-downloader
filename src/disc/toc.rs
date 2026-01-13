//! Table of Contents (TOC) extraction and disc ID calculation
//!
//! Implements MusicBrainz DiscID calculation from CUE sheet data.

use sha1::{Sha1, Digest};
use base64::Engine;

/// Frames per second (CD audio)
const FRAMES_PER_SECOND: u32 = 75;

/// TOC (Table of Contents) data for an audio CD
#[derive(Debug, Clone)]
pub struct DiscTOC {
    /// First track number (usually 1)
    pub first_track: u8,
    /// Last track number
    pub last_track: u8,
    /// Lead-out offset in frames
    pub lead_out: u32,
    /// Track offsets in frames (LSN - Logical Sector Number)
    pub track_offsets: Vec<u32>,
}

/// Information about a single track from CUE
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// Track number
    pub number: u8,
    /// Offset in frames (MM:SS:FF from CUE INDEX 01)
    pub offset: u32,
    /// Track type (AUDIO, MODE1, etc.)
    pub track_type: String,
}

impl DiscTOC {
    /// Create a new TOC from track information
    pub fn from_tracks(tracks: Vec<TrackInfo>, total_length_frames: u32) -> Option<Self> {
        if tracks.is_empty() {
            return None;
        }

        let first_track = tracks.first()?.number;
        let last_track = tracks.last()?.number;
        let track_offsets: Vec<u32> = tracks.iter().map(|t| t.offset + 150).collect(); // +150 for pregap
        let lead_out = total_length_frames + 150;

        Some(Self {
            first_track,
            last_track,
            lead_out,
            track_offsets,
        })
    }

    /// Calculate MusicBrainz DiscID
    /// 
    /// Reference: https://musicbrainz.org/doc/Disc_ID_Calculation
    pub fn calculate_musicbrainz_id(&self) -> String {
        let mut hasher = Sha1::new();

        // Build binary data: first track (1 byte) + last track (1 byte) + lead-out (4 bytes BE) + track offsets (99 * 4 bytes BE)
        let mut data = Vec::with_capacity(2 + 4 + 99 * 4);
        
        // First track number
        data.push(self.first_track);
        
        // Last track number
        data.push(self.last_track);
        
        // Lead-out offset (4 bytes, big-endian)
        data.extend_from_slice(&self.lead_out.to_be_bytes());
        
        // Track offsets (99 tracks, padded with zeros)
        for i in 0..99 {
            let offset = if i < self.track_offsets.len() {
                self.track_offsets[i]
            } else {
                0
            };
            data.extend_from_slice(&offset.to_be_bytes());
        }

        // Hash the binary data
        hasher.update(&data);
        let result = hasher.finalize();

        // Use standard base64url encoding (RFC 4648) without padding
        // MusicBrainz uses: A-Z, a-z, 0-9, - (minus), _ (underscore)
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&result)
    }

    /// Calculate FreeDB DiscID (legacy, but still used)
    /// 
    /// Reference: http://ftp.freedb.org/pub/freedb/latest/CDDBPROTO
    pub fn calculate_freedb_id(&self) -> String {
        let mut checksum = 0u32;

        // Calculate checksum from track offsets (in seconds)
        for offset in &self.track_offsets {
            let seconds = offset / FRAMES_PER_SECOND;
            checksum += digit_sum(seconds);
        }

        let total_seconds = self.lead_out / FRAMES_PER_SECOND;
        let first_offset_seconds = self.track_offsets.first().unwrap_or(&0) / FRAMES_PER_SECOND;
        let length = total_seconds - first_offset_seconds;

        let disc_id = ((checksum % 0xFF) << 24)
            | (length << 8)
            | self.track_offsets.len() as u32;

        format!("{:08x}", disc_id)
    }

    /// Get total disc length in seconds
    pub fn total_seconds(&self) -> u32 {
        self.lead_out / FRAMES_PER_SECOND
    }

    /// Get total disc length formatted as MM:SS
    pub fn total_time_string(&self) -> String {
        let seconds = self.total_seconds();
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }

    /// Get number of tracks
    pub fn track_count(&self) -> u8 {
        self.last_track - self.first_track + 1
    }

    /// Generate TOC string for MusicBrainz fuzzy lookup
    /// Format: first_track + track_count + leadout + offset1 + offset2 + ...
    /// Example: 1+12+267257+150+22767+41887+...
    pub fn to_toc_string(&self) -> String {
        let mut parts = vec![
            self.first_track.to_string(),
            self.track_count().to_string(),
            self.lead_out.to_string(),
        ];
        
        for offset in &self.track_offsets {
            parts.push(offset.to_string());
        }
        
        parts.join("+")
    }
}

/// Parse MSF (Minutes:Seconds:Frames) time from CUE INDEX format
/// Format: MM:SS:FF where FF is frames (0-74)
pub fn parse_msf(msf: &str) -> Option<u32> {
    let parts: Vec<&str> = msf.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let minutes: u32 = parts[0].parse().ok()?;
    let seconds: u32 = parts[1].parse().ok()?;
    let frames: u32 = parts[2].parse().ok()?;

    Some((minutes * 60 + seconds) * FRAMES_PER_SECOND + frames)
}

/// Sum of decimal digits (for FreeDB checksum)
fn digit_sum(mut n: u32) -> u32 {
    let mut sum = 0;
    while n > 0 {
        sum += n % 10;
        n /= 10;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_msf() {
        assert_eq!(parse_msf("00:00:00"), Some(0));
        assert_eq!(parse_msf("00:02:00"), Some(150));
        assert_eq!(parse_msf("01:00:00"), Some(4500));
        assert_eq!(parse_msf("74:59:74"), Some(337424));
    }

    #[test]
    fn test_digit_sum() {
        assert_eq!(digit_sum(0), 0);
        assert_eq!(digit_sum(123), 6);
        assert_eq!(digit_sum(999), 27);
    }

    #[test]
    fn test_toc_calculation() {
        let tracks = vec![
            TrackInfo {
                number: 1,
                offset: 0,
                track_type: "AUDIO".to_string(),
            },
            TrackInfo {
                number: 2,
                offset: 18901,
                track_type: "AUDIO".to_string(),
            },
        ];

        let toc = DiscTOC::from_tracks(tracks, 41000).unwrap();
        assert_eq!(toc.first_track, 1);
        assert_eq!(toc.last_track, 2);
        assert_eq!(toc.track_count(), 2);
    }
}
