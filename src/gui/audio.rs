//! Background CD-DA playback for the disc-info panel.
//!
//! Playing a CHD audio track means first extracting the whole disc to a
//! temporary BIN (see [`crate::disc::chd_audio`]), which is slow, so all the
//! work runs on a dedicated thread. The UI polls [`AudioPlayback::state`] each
//! frame and offers a Stop button.
//!
//! rodio's `OutputStream` is `!Send`, so it is created and owned entirely on
//! the playback thread; the UI only ever touches the shared state flag.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rodio::buffer::SamplesBuffer;

use crate::disc::chd_audio::{self, CDDA_CHANNELS, CDDA_SAMPLE_RATE};

/// How far ahead of the speaker we let the queue run before applying
/// backpressure (each queued chunk is ~1 second).
const MAX_QUEUED_CHUNKS: usize = 4;

/// Lifecycle of a single track's playback.
#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackState {
    /// Extracting the disc to a temp BIN — no audio yet.
    Preparing,
    /// Audio is streaming to the output device.
    Playing,
    /// Finished normally or stopped by the user.
    Finished,
    /// Failed; carries a human-readable message.
    Failed(String),
}

/// Handle to a background playback job for one audio track. Dropping it (or
/// calling [`AudioPlayback::stop`]) signals the worker to tear down.
pub struct AudioPlayback {
    track: u32,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<PlaybackState>>,
}

impl AudioPlayback {
    /// Spawn a worker that extracts and plays `track` from `chd_path`.
    pub fn start(chd_path: PathBuf, track: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(PlaybackState::Preparing));
        let stop_bg = Arc::clone(&stop);
        let state_bg = Arc::clone(&state);

        // Detached: the worker owns its rodio stream and the temp extraction,
        // and tears itself down when it sees the stop flag or finishes.
        thread::spawn(move || run(chd_path, track, &stop_bg, &state_bg));

        Self { track, stop, state }
    }

    /// The track number this job is playing.
    pub fn track(&self) -> u32 {
        self.track
    }

    /// Current lifecycle state (cheap; clones a small enum).
    pub fn state(&self) -> PlaybackState {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(PlaybackState::Finished)
    }

    /// Whether the job is still preparing or playing.
    pub fn is_active(&self) -> bool {
        matches!(
            self.state(),
            PlaybackState::Preparing | PlaybackState::Playing
        )
    }

    /// Signal the worker to stop. Extraction in progress stops at the next
    /// chunk boundary; queued audio is cut immediately.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

fn set(state: &Arc<Mutex<PlaybackState>>, value: PlaybackState) {
    if let Ok(mut s) = state.lock() {
        *s = value;
    }
}

fn run(chd_path: PathBuf, track: u32, stop: &Arc<AtomicBool>, state: &Arc<Mutex<PlaybackState>>) {
    let (_stream, handle) = match rodio::OutputStream::try_default() {
        Ok(out) => out,
        Err(e) => return set(state, PlaybackState::Failed(format!("no audio output: {e}"))),
    };
    let sink = match rodio::Sink::try_new(&handle) {
        Ok(s) => s,
        Err(e) => return set(state, PlaybackState::Failed(format!("audio sink: {e}"))),
    };

    let mut started = false;
    let result = chd_audio::extract_audio_pcm(&chd_path, track, |samples| {
        if !started {
            set(state, PlaybackState::Playing);
            started = true;
        }
        // Bounded queue: wait while the sink is a few chunks ahead, bailing if
        // the user hit Stop.
        while sink.len() >= MAX_QUEUED_CHUNKS {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        sink.append(SamplesBuffer::new(
            CDDA_CHANNELS,
            CDDA_SAMPLE_RATE,
            samples.to_vec(),
        ));
    });

    if let Err(e) = result {
        // A stop during extraction surfaces as a short-read/None, not a real
        // failure — report Finished in that case.
        if stop.load(Ordering::Relaxed) {
            set(state, PlaybackState::Finished);
        } else {
            set(state, PlaybackState::Failed(e));
        }
        return;
    }

    // Drain the queued audio, honoring Stop.
    loop {
        if stop.load(Ordering::Relaxed) {
            sink.stop();
            break;
        }
        if sink.empty() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    set(state, PlaybackState::Finished);
}
