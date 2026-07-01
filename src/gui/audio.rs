//! Background CD-DA playback for the disc-info panel.
//!
//! Playing an audio track (see [`crate::disc::cd_audio`]) can be slow to start —
//! a CHD extracts the whole disc to a temporary BIN first — so all the work runs
//! on a dedicated thread. The UI polls [`AudioPlayback::state`] each frame and
//! offers a Stop button.
//!
//! rodio's `OutputStream` is `!Send`, so it is created and owned entirely on
//! the playback thread; the UI only ever touches the shared state flag.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;

use crate::disc::cd_audio::{self, CDDA_CHANNELS, CDDA_SAMPLE_RATE};

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
    /// Set to `Instant::now()` the moment the first audio reaches the sink, i.e.
    /// when playback actually starts. `None` while still `Preparing`.
    play_start: Arc<Mutex<Option<Instant>>>,
}

impl AudioPlayback {
    /// Spawn a worker that extracts and plays `track` from `image_path`.
    pub fn start(image_path: PathBuf, track: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(PlaybackState::Preparing));
        let play_start = Arc::new(Mutex::new(None));
        let stop_bg = Arc::clone(&stop);
        let state_bg = Arc::clone(&state);
        let play_start_bg = Arc::clone(&play_start);

        // Detached: the worker owns its rodio stream and the temp extraction,
        // and tears itself down when it sees the stop flag or finishes.
        thread::spawn(move || run(image_path, track, &stop_bg, &state_bg, &play_start_bg));

        Self {
            track,
            stop,
            state,
            play_start,
        }
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

    /// Seconds of audio played so far, or `None` while still preparing.
    ///
    /// CD-DA streams to the sink in real time, so wall-clock since the first
    /// sample was queued tracks the speaker position (bar a few ms of startup
    /// latency). Callers should clamp to the track length for display.
    pub fn elapsed_secs(&self) -> Option<f64> {
        self.play_start
            .lock()
            .ok()
            .and_then(|p| p.map(|t| t.elapsed().as_secs_f64()))
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

fn run(
    image_path: PathBuf,
    track: u32,
    stop: &Arc<AtomicBool>,
    state: &Arc<Mutex<PlaybackState>>,
    play_start: &Arc<Mutex<Option<Instant>>>,
) {
    let (_stream, handle) = match rodio::OutputStream::try_default() {
        Ok(out) => out,
        Err(e) => return set(state, PlaybackState::Failed(format!("no audio output: {e}"))),
    };
    let sink = match rodio::Sink::try_new(&handle) {
        Ok(s) => s,
        Err(e) => return set(state, PlaybackState::Failed(format!("audio sink: {e}"))),
    };

    let mut started = false;
    let result = cd_audio::extract_audio_pcm(&image_path, track, |samples| {
        if !started {
            if let Ok(mut p) = play_start.lock() {
                *p = Some(Instant::now());
            }
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
