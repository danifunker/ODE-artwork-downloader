# Wire ODE's CHD audio player to `cd-da-reader`'s file backing

> **Task prompt** for a Claude Code session working in this repo
> (`ODE-artwork-downloader`). Self-contained: follow it top to bottom, then run
> the verification steps before reporting done. Line numbers are approximate —
> `rg` for symbols. Work on the `audio-chd-player` branch (or a branch off it).

## Goal

The `audio-chd-player` branch has a CHD CD-DA player whose adapter
(`src/disc/chd_audio.rs`) was written against a **nonexistent** `rust_cdda`
crate (`CdImage::open`, `img.tracks()`, `img.audio_reader().read_samples()`),
so the branch **does not build**. That crate never shipped.

The real crate is **`cd-da-reader`** (sibling repo `../rust-cd-da-reader`), and
it has taken the opposite, cleaner design: **it does not bake in any image
format**. It defines a small trait, `AudioSectorReader`, and *we* (ODE) supply
the CHD decoding — using the `libchdman-rs` we **already link**. `cd-da-reader`
then reuses its `Track`/`Toc` types, track-bounds math (incl. the CD-Extra
gap), and WAV helper over our sectors.

Rewrite `src/disc/chd_audio.rs` to this real API. Keep the adapter's **public
surface unchanged** so `examples/play_chd.rs` and `src/gui/audio.rs` need
little-to-no change.

## Why this design is better for us (read once, then act)

Because `cd-da-reader` no longer depends on `libchdman-rs` at all, there is **no
`links` single-copy coordination** between it and ODE anymore. The only
`libchdman-rs` copies in our graph are our own direct dep and the one
`opticaldiscs` pulls in — already aligned at `^0.288`. Adding `cd-da-reader`
brings in **zero** new native deps.

## The `cd-da-reader` API you get (already implemented, verify with `rg`)

```rust
// Format-agnostic types + helpers (all in cd_da_reader crate root):
pub struct Track { pub number: u8, pub start_lba: u32, pub start_msf: (u8,u8,u8), pub is_audio: bool }
pub struct Toc   { pub first_track: u8, pub last_track: u8, pub tracks: Vec<Track>, pub leadout_lba: u32 }

pub fn lba_to_msf(lba: u32) -> (u8, u8, u8);         // fill Track.start_msf
pub fn create_wav(pcm: Vec<u8>) -> Vec<u8>;          // prepend 44-byte RIFF header
pub fn get_track_bounds(toc: &Toc, n: u8) -> std::io::Result<(u32 /*start_lba*/, u32 /*sectors*/)>;

// The seam we implement:
pub trait AudioSectorReader {
    type Error;
    /// Return exactly `count * 2352` bytes of little-endian 16-bit stereo PCM.
    fn read_audio_sectors(&self, start_lba: u32, count: u32) -> Result<Vec<u8>, Self::Error>;
}

// Generic track read over any backing (the file counterpart to CdReader::read_track):
pub fn read_track<R: AudioSectorReader>(src: &R, toc: &Toc, track_no: u8)
    -> Result<Vec<u8>, TrackReadError<R::Error>>;

pub enum TrackReadError<E> { Toc(std::io::Error), Backend(E) } // impls Error/Display when E does
```

Sector format contract: **2352 bytes/sector, 16-bit signed little-endian,
stereo, 44100 Hz** — identical to a physical rip. `start_lba` is an absolute LBA
(sector index, LBA 0 = first sector after lead-in).

There is a complete, dependency-free usage example at
`../rust-cd-da-reader/examples/file_backend.rs` — read it first.

## Cargo.toml

Add the path dep (**no features** — the `chd` feature idea was dropped; the
crate is format-agnostic now):

```toml
cd-da-reader = { path = "../rust-cd-da-reader" }
```

Keep the existing `libchdman-rs = { version = "0.288.8", features = ["prebuilt"] }`
and `opticaldiscs` deps as-is — we still use `libchdman-rs` directly to decode
CHD, and `opticaldiscs` for the data-track/hashing path (unaffected).

## How to decode CHD → sectors (this is the part we own now)

Use `libchdman-rs` exactly as the *old* `cd-da-reader` design doc described
(`../rust-cd-da-reader/docs/add_chd_cd_da_reading.md`), but living here in ODE:

```rust
use libchdman_rs::Chd;
use libchdman_rs::cd::{list_tracks, extract_to_cue, TrackType, TrackInfo};
```

- `Chd::open(path, /*writeable=*/ false, /*parent=*/ None)?`
- `list_tracks(&chd)? -> Vec<TrackInfo>` — `TrackInfo { track_num, track_type, frames, pregap, ... }`; `TrackType::Audio` are the CD-DA tracks.
- **Robust PCM path:** `extract_to_cue(chd_path, &cue_path, &bin_path, &mut on_progress)?` writes each audio track as **2352-byte little-endian** sectors — it strips the 96-byte subcode and byte-swaps CHD's big-endian CD-DA back to LE for us. Do **not** use `CdCookedReader` (it rejects audio tracks).
- **Endian:** `extract_to_cue` already yields LE. If you ever switch to raw `Chd::read_bytes`, *you* own the 16-bit LE swap and the 2448→2352 subcode strip — get it right against `extract_to_cue` output.

`rg` the actual signatures in the `libchdman-rs` source (or its docs) before
calling — the arg order/`&mut` on the progress closure matters.

## Implementation sketch for `src/disc/chd_audio.rs` (extract-based, robust first)

Define an internal backing that owns the extracted BIN + a `cd_da_reader::Toc`,
and implements `AudioSectorReader`:

```rust
use std::path::Path;
use cd_da_reader::{AudioSectorReader, Toc, Track, lba_to_msf, read_track};

struct ChdDisc {
    bin_path: std::path::PathBuf,   // temp redump-style BIN from extract_to_cue
    _tmp: /* tempfile::TempDir or manual cleanup guard */,
    toc: Toc,
}

impl ChdDisc {
    fn open(chd: &Path) -> Result<Self, String> {
        let chd_handle = Chd::open(chd.to_str().unwrap(), false, None).map_err(|e| e.to_string())?;
        let tracks = list_tracks(&chd_handle).map_err(|e| e.to_string())?;

        // Extract the whole disc to a temp BIN (LE, 2352 B/sector).
        let dir = std::env::temp_dir().join(/* unique subdir */);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let bin_path = dir.join("disc.bin");
        let cue_path = dir.join("disc.cue");
        let mut on_progress = |_written: u64| {};
        extract_to_cue(chd, &cue_path, &bin_path, &mut on_progress).map_err(|e| e.to_string())?;

        // Build cd-da-reader's Toc from cumulative frame offsets.
        let mut cd_tracks = Vec::new();
        let mut lba = 0u32;
        for t in &tracks {
            cd_tracks.push(Track {
                number: t.track_num as u8,           // CDs have <= 99 tracks
                start_lba: lba,
                start_msf: lba_to_msf(lba),
                is_audio: t.track_type == TrackType::Audio,
            });
            lba += t.frames;                          // NB: chdman may pad track lengths — verify
        }
        let toc = Toc { first_track: /* min */, last_track: /* max */, tracks: cd_tracks, leadout_lba: lba };

        Ok(Self { bin_path, _tmp: /* guard */, toc })
    }
}

impl AudioSectorReader for ChdDisc {
    type Error = std::io::Error;
    fn read_audio_sectors(&self, start_lba: u32, count: u32) -> Result<Vec<u8>, std::io::Error> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.bin_path)?;
        f.seek(SeekFrom::Start(start_lba as u64 * 2352))?;
        let mut buf = vec![0u8; count as usize * 2352];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}
```

**Watch the frame math**: `chdman` can pad CHD track lengths, and the extracted
BIN's per-track offsets must line up with the LBAs you put in the `Toc`. Verify
that `read_track(&disc, &disc.toc, n)` byte-range matches what `chdman extractcd`
produces for track `n` (the `play_chd --save-wav` diff below is exactly this
check). If offsets drift, reconcile against the `.cue` `extract_to_cue` wrote.

### Keep the public surface stable

`src/disc/mod.rs` re-exports `ChdCdTrack, CDDA_CHANNELS, CDDA_SAMPLE_RATE`, and
`play_chd.rs` / `gui/audio.rs` call `chd_audio::read_tracks` and
`chd_audio::extract_audio_pcm`. Preserve all of these:

- `pub const CDDA_SAMPLE_RATE: u32 = 44_100;` / `pub const CDDA_CHANNELS: u16 = 2;` — unchanged.
- `pub struct ChdCdTrack { number: u32, track_type: String, frames: u32, pregap: u32, is_audio: bool }` and `duration_mmss()` — unchanged; populate it from `list_tracks` (or from the `Toc` + `TrackInfo`).
- `pub fn read_tracks(path: &Path) -> Result<Vec<ChdCdTrack>, String>` — reimplement on `list_tracks` (or `ChdDisc::open` + its `Toc`).
- `pub fn extract_audio_pcm<F: FnMut(&[i16])>(path, track_number: u32, on_samples: F) -> Result<u64, String>` — reimplement:
  1. `let disc = ChdDisc::open(path)?;`
  2. `let pcm = read_track(&disc, &disc.toc, track_number as u8).map_err(|e| e.to_string())?;` (error on non-audio: check `ChdCdTrack.is_audio` first, matching current behavior).
  3. Convert the LE byte `Vec<u8>` to `i16` and emit in ~1-second batches to keep the callback contract (`for chunk in pcm.chunks(1176*75) { emit i16s }`). Convert bytes→i16 portably with `i16::from_le_bytes([b0,b1])` (do **not** rely on host endianness). Return the total `i16` sample count.

A single track is bounded (~30 MB for 3 min), so reading the whole track before
emitting is fine. If you want the old low-latency start / flat memory, loop
`disc.read_audio_sectors(lba, chunk)` over the track's sector range instead of
`read_track` — same result, streamed. Note this trade-off in a comment.

`gui/audio.rs` already models this as "Preparing" (the slow whole-disc extract)
→ "Playing"; that maps cleanly: `ChdDisc::open` is the Preparing cost, the
per-track read is fast.

## Verification (run all; report output)

```sh
cargo build
cargo clippy --all-targets -- -D warnings
cargo build --example play_chd
cargo run --release --example play_chd -- <path-to.chd>            # lists tracks, plays first audio
cargo run --release --example play_chd -- <path-to.chd> 2 --save-wav /tmp/t2.wav
cargo tree -i libchdman-rs                                         # MUST show a single version
cargo run --release --example smoke_chd -- <path-to.chd>          # unaffected data path still works
```

- Play a track: correct pitch/speed and no static ⇒ endianness/extraction are right.
- Diff `/tmp/t2.wav` (minus its 44-byte header) against `chdman extractcd` for the same track ⇒ byte-order and per-track offsets are correct.
- `cargo tree -i libchdman-rs` shows exactly **one** version (our direct pin + opticaldiscs's transitive one agree; `cd-da-reader` adds none).

## Done criteria

- `audio-chd-player` builds; `chd_audio.rs` uses `cd-da-reader`'s `AudioSectorReader` + `read_track`, with ODE decoding CHD via its own `libchdman-rs`.
- `play_chd.rs` and `gui/audio.rs` compile and run against the unchanged adapter surface (`read_tracks`, `extract_audio_pcm`, `ChdCdTrack`, the two constants).
- A dumped WAV matches `chdman extractcd` for the same track; playback sounds correct.
- `cargo tree -i libchdman-rs` shows a single version.

## Follow-ups (note, don't build unless asked)

- **Cache the extraction**: `ChdDisc::open` re-extracts the whole disc on every `extract_audio_pcm` call. Cache the temp BIN per CHD path (or open the `ChdDisc` once in `gui/audio.rs` and reuse across tracks).
- **Streaming decode** via raw `Chd::read_bytes` (no whole-disc temp BIN): strip the 96-byte subcode, swap 16-bit samples to LE, own the per-track frame-offset math. Validate against `extract_to_cue` output.
- **BIN/CUE inputs**: a `ChdDisc`-like backing that reads audio sectors straight from an existing BIN (already LE, no CHD step) behind the same `AudioSectorReader`.
```
