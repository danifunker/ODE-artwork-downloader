# Bump opticaldiscs 0.16 + libchdman-rs 0.289.3; stream CHD audio with `CdAudioReader`

> **Task prompt** for a Claude Code session in this repo (`ODE-artwork-downloader`).
> Work through it in order and run the verification steps before you report done.
> Line numbers are approximate, so use `rg` to find symbols.

## Before you start: the working tree is dirty

When this prompt was written, `main` had **uncommitted** dependency upgrades
(egui/eframe 0.37, rodio 0.23, rusqlite 0.41, opticaldiscs 0.15, and others)
in `Cargo.toml`, `Cargo.lock`, `build.rs`, `examples/play_chd.rs`,
`src/gui/audio.rs`, `src/gui/app.rs`, `src/gui/browse_view.rs` and
`src/crypto.rs`. That is the user's work in progress. **Do not discard or
overwrite it.** Run `git status`/`git diff` first. If it is still uncommitted,
ask the user whether to build on top of it or wait until it has been committed.
The steps below assume you build on top of it.

## Goal

1. Move to **opticaldiscs 0.16** and **libchdman-rs 0.289.3** together.
2. Replace the CHD audio path in `src/disc/cd_audio.rs`. Today it extracts the
   whole disc to a temp BIN with `cd::extract_to_cue` and then slices that BIN.
   The new path streams straight from the CHD with
   `libchdman_rs::cd::CdAudioReader`.

Do (1) and (2) as **separate commits**.

## 1. Dependency bump

opticaldiscs 0.16.0 is published. Its only change is moving its own
`libchdman-rs` dependency from 0.288 to `0.289.3`. libchdman-rs is a `links`
crate, so the build graph can hold only **one** copy of it. ODE's direct pin
must therefore move in lockstep, because `0.288.9` will not resolve next to
opticaldiscs 0.16. No opticaldiscs API changed between 0.15 and 0.16, and
nothing ODE already uses from libchdman-rs changed between 0.288 and 0.289
(`Chd`, `cd::list_tracks`, `cd::TrackType`, `cd::extract_to_cue`).

In `Cargo.toml`:

```toml
opticaldiscs = { version = "0.16", features = ["toc"] }
libchdman-rs = { version = "0.289.3", features = ["prebuilt"] }
```

- Also update the stale comment above the libchdman-rs line. It still says
  `opticaldiscs 0.6.x -> 0.288.x`; make it say `opticaldiscs 0.16.x -> 0.289.x`
  and keep the "bump both together" warning. Fix the matching comment above the
  `opticaldiscs` line too (it says "0.6.x uses libchdman-rs").
- Run `cargo update -p opticaldiscs -p libchdman-rs`. Then check
  `cargo tree -i libchdman-rs`: it should show exactly one copy, 0.289.x.
- Build and test. This step should need no code changes. If something breaks,
  it most likely comes from the uncommitted 0.6→0.15 jump, not from 0.16. See
  opticaldiscs' `CHANGELOG.md` (sibling repo `../opticaldiscs-rs`).
- Commit.

## 2. CHD audio through `CdAudioReader`

### Why

`ChdDisc::open` currently decompresses the **whole disc** into a temp dir before
a single sample plays, which is the slow "Preparing" step. It also holds the
whole track in memory: `read_track_with_bounds` returns one `Vec`. Finally, its
TOC is built from `list_tracks` frame counts, which only approximates the
extracted BIN's layout (see the `NB:` comment in `ChdDisc::open`).
`CdAudioReader` reads sectors on demand from the CHD, with the same bytes
`extract_to_cue` would write. That removes all three problems.

### The API (libchdman-rs 0.289.3, `libchdman_rs::cd`)

Check these with `rg` in `~/.cargo/registry/src/*/libchdman-rs-0.289.3/src/cd.rs`.

```rust
pub struct CdAudioReader { /* owns the Chd */ }   // Send, NOT Sync
impl CdAudioReader {
    pub const SECTOR_SIZE: usize = 2352;
    /// Whole disc in extract_to_cue's single-BIN address space.
    pub fn open(chd: Chd) -> Result<Self>;
    /// One track (0-based index); sector 0 = track's first emitted frame,
    /// i.e. exactly that track's slice of the extracted .bin.
    pub fn open_track(chd: Chd, track_index: u32) -> Result<Self>;
    pub fn sector_count(&self) -> u32;
    pub fn len(&self) -> u64;
    /// buf.len() must equal count * 2352. Audio is little-endian, subcode dropped.
    pub fn read_sectors(&mut self, start: u32, count: u32, buf: &mut [u8]) -> Result<()>;
    pub fn into_inner(self) -> Chd;
}
// also impl Read + Seek
```

Errors: `ChdError::NotCdMedia` for a non-CD CHD. `InvalidData` for a bad track
index, a sector range out of bounds, or a wrong-size buffer.
`UnsupportedFormat` when the view covers a **cooked** data track
(`MODE1/2048` and the like).

**Use `open_track`, not `open`.** `open` rejects the *whole disc* if any track
is cooked. A mixed-mode CHD with a cooked data track would then lose audio
playback entirely. `open_track` only cares about the track you ask for.

### cd-da-reader side (1.0.0, already a dependency)

`AudioSectorReader::read_audio_sectors` takes `&self`, but `read_sectors` takes
`&mut self`, so wrap the reader in a `Mutex`:

```rust
struct ChdTrackAudio(std::sync::Mutex<CdAudioReader>);

impl AudioSectorReader for ChdTrackAudio {
    type Error = String; // or a small error type
    fn read_audio_sectors(&self, start_lba: u32, count: u32) -> Result<Vec<u8>, Self::Error> {
        let mut buf = vec![0u8; count as usize * CdAudioReader::SECTOR_SIZE];
        self.0.lock().map_err(|_| "CHD reader poisoned".to_string())?
            .read_sectors(start_lba, count, &mut buf)
            .map_err(|e| format!("read CHD sectors: {e:?}"))?;
        Ok(buf)
    }
}
```

With a per-track view the track's bounds are simply `0 .. sector_count()`, so
you no longer need a `Toc`. Use the zero-policy stream primitive:

```rust
cd_da_reader::open_track_stream_at(&src, 0, sector_count)  // -> AudioTrackStream
    .with_sectors_per_chunk(75)                             // ~1 s per chunk
// loop: while let Some(bytes) = stream.next_chunk()? { on_samples(&pcm_le_i16(&bytes)) }
```

(`open_track_stream_with_bounds(src, &toc, n, TrackBounds::Gapless)` is the
alternative if you keep the whole-disc `open` + `Toc` approach. Don't take it,
for the cooked-track reason above. Neither choice reintroduces the CD-Extra gap
bug that `TrackBounds::Gapless` was fixing.)

### Changes in `src/disc/cd_audio.rs`

- `extract_audio_pcm_chd`: open the CHD, find the track in `list_tracks` by its
  1-based `track_num`, and keep the "not an AUDIO track" guard. Then call
  `CdAudioReader::open_track(chd, index)` with the **0-based index into
  `list_tracks`**; don't assume it equals `track_num - 1`. Stream with
  `open_track_stream_at` and emit chunks to `on_samples` as you read them,
  instead of one big `Vec`. Keep ~1 s batches: the player's backpressure in
  `gui/audio.rs` and `examples/play_chd.rs` counts queued chunks.
- Delete `ChdDisc`, its `impl AudioSectorReader`, and the now-unused imports
  (`extract_to_cue`, `read_track_with_bounds`, `TrackBounds`, `Toc`, `Track`,
  `lba_to_msf`, and `Seek`/`SeekFrom` if nothing else uses them).
- **Leave `src/disc/hasher.rs` alone.** It still uses `extract_to_cue` +
  `tempfile` to hash a redump-style BIN, which is a separate concern. `tempfile`
  stays a dependency because the hasher uses it.
- Rewrite the module doc comment: CHD now streams on demand, there is no temp
  BIN, and `TrackBounds::Gapless` is no longer involved. Keep the BIN/CUE
  backing and its docs as they are.
- `read_tracks_chd` can stay as it is.
- The "Preparing" state in the GUI (`gui/audio.rs`, `gui/app.rs`) should now
  last only a moment for CHDs. Adjust any wording that promises a long
  extraction, but don't redesign the UI.

### Verify

1. `cargo build`, `cargo test`, and `cargo clippy -- -D warnings`.
2. **Byte-for-byte check against the old path.** Before step 2, build the
   example and save a WAV of one audio track from a real CHD (`cargo run
   --example play_chd -- <file.chd> <track-number> --save-wav before.wav`; the
   track number is a bare positional argument). Do this for a pure-audio CHD and,
   if you have one, a mixed-mode (data + audio) CHD. After step 2, dump
   `after.wav` the same way and `cmp` them; they must be identical. If they
   differ, they most likely differ in **length**: `open_track` includes any
   pregap the CHD stores, while the old TOC used `list_tracks` `frames`.
   Report the difference to the user rather than trimming it to match
   silently. Ask the user for test CHD paths if none are on hand.
3. Run the GUI, play a CHD track, and confirm playback starts almost at once.
4. Commit step 2 separately from the dependency bump.

## Out of scope

- Seeking or a scrub bar (`AudioTrackStream::seek_to_seconds` makes it easy
  later; just don't build it now).
- Moving BIN/CUE onto `AudioSectorReader`.
- Changing how the hasher extracts CHDs.
