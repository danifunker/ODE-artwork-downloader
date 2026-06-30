//! Play (or dump) a CD-DA audio track from a MAME-style `.chd` CD image.
//!
//! This is a verification/debug tool: it exercises the same CHD audio
//! extraction path the app uses ([`disc::chd_audio`]) so we can *hear* that the
//! PCM is correct — right pitch/speed, no static — and diff it against a
//! `chdman extractcd` rip.
//!
//! Usage:
//!   cargo run --release --example play_chd -- <path-to-chd> [track] [--save-wav <file>]
//!
//! Examples:
//!   # List tracks and play the first AUDIO track:
//!   cargo run --release --example play_chd -- game.chd
//!
//!   # Play track 3 specifically:
//!   cargo run --release --example play_chd -- game.chd 3
//!
//!   # Dump track 2 to a 44.1kHz/16-bit/stereo WAV instead of playing:
//!   cargo run --release --example play_chd -- game.chd 2 --save-wav track2.wav
//!
//! The `--save-wav` output should be bit-identical (modulo the WAV header) to
//! `chdman extractcd` for the same track, confirming byte order and sector
//! extraction are correct.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use ode_artwork_downloader::disc::chd_audio::{
    self, ChdCdTrack, CDDA_CHANNELS, CDDA_SAMPLE_RATE,
};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprintln!(
                "usage: play_chd <path-to-chd> [track] [--save-wav <file>]"
            );
            std::process::exit(2);
        }
    };

    if !args.chd.exists() {
        eprintln!("file does not exist: {}", args.chd.display());
        std::process::exit(2);
    }

    if let Err(e) = run(&args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let tracks = chd_audio::read_tracks(&args.chd)?;
    print_track_table(&tracks);

    let track = select_track(&tracks, args.track)?;

    if let Some(wav_path) = &args.save_wav {
        save_wav(&args.chd, track, wav_path)
    } else {
        play(&args.chd, track)
    }
}

/// Print the parsed track table so the user can see what's on the disc.
fn print_track_table(tracks: &[ChdCdTrack]) {
    println!("\nTracks:");
    println!("  {:>3}  {:<10}  {:>8}  {:>8}", "#", "TYPE", "FRAMES", "LENGTH");
    for t in tracks {
        println!(
            "  {:>3}  {:<10}  {:>8}  {:>8}",
            t.number,
            t.track_type,
            t.frames,
            t.duration_mmss()
        );
    }
    println!();
}

/// Resolve which track to use: the requested number, or the first AUDIO track.
fn select_track(tracks: &[ChdCdTrack], requested: Option<u8>) -> Result<&ChdCdTrack, String> {
    match requested {
        Some(num) => {
            let track = tracks
                .iter()
                .find(|t| t.number == num as u32)
                .ok_or_else(|| format!("no track {num} on this disc"))?;
            if !track.is_audio {
                return Err(format!(
                    "track {num} is {}, not an AUDIO track",
                    track.track_type
                ));
            }
            Ok(track)
        }
        None => tracks
            .iter()
            .find(|t| t.is_audio)
            .ok_or_else(|| "no AUDIO track on this disc".to_string()),
    }
}

/// Stream the track to the default audio device and play to completion.
fn play(chd: &Path, track: &ChdCdTrack) -> Result<(), String> {
    use rodio::buffer::SamplesBuffer;

    println!(
        "Playing track {} ({}, {})…",
        track.number,
        track.track_type,
        track.duration_mmss()
    );

    let (_stream, handle) =
        rodio::OutputStream::try_default().map_err(|e| format!("open audio output: {e}"))?;
    let sink = rodio::Sink::try_new(&handle).map_err(|e| format!("create audio sink: {e}"))?;

    // `extract_audio_pcm` hands us ~1s batches; queue each on the sink with
    // backpressure so we never buffer more than a few seconds of the (possibly
    // hundreds of MB) track ahead of playback.
    const MAX_QUEUED_CHUNKS: usize = 4;

    chd_audio::extract_audio_pcm(chd, track.number, |samples| {
        while sink.len() >= MAX_QUEUED_CHUNKS {
            thread::sleep(Duration::from_millis(20));
        }
        sink.append(SamplesBuffer::new(
            CDDA_CHANNELS,
            CDDA_SAMPLE_RATE,
            samples.to_vec(),
        ));
    })?;

    sink.sleep_until_end();
    println!("Done.");
    Ok(())
}

/// Write the track's PCM as a 44.1kHz/16-bit/stereo WAV.
fn save_wav(chd: &Path, track: &ChdCdTrack, out: &Path) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: CDDA_CHANNELS,
        sample_rate: CDDA_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(out, spec).map_err(|e| format!("create {}: {e}", out.display()))?;

    println!(
        "Writing track {} ({}) to {}…",
        track.number,
        track.duration_mmss(),
        out.display()
    );

    let mut write_err: Option<String> = None;
    let total = chd_audio::extract_audio_pcm(chd, track.number, |samples| {
        if write_err.is_some() {
            return;
        }
        for &s in samples {
            if let Err(e) = writer.write_sample(s) {
                write_err = Some(format!("write sample: {e}"));
                return;
            }
        }
    })?;

    if let Some(e) = write_err {
        return Err(e);
    }

    writer
        .finalize()
        .map_err(|e| format!("finalize {}: {e}", out.display()))?;

    println!(
        "Wrote {} samples ({} bytes of PCM).",
        total,
        total * 2
    );
    Ok(())
}

/// Parsed command-line arguments.
struct Args {
    chd: PathBuf,
    track: Option<u8>,
    save_wav: Option<PathBuf>,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut chd: Option<PathBuf> = None;
        let mut track: Option<u8> = None;
        let mut save_wav: Option<PathBuf> = None;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--save-wav" => {
                    let file = args
                        .next()
                        .ok_or_else(|| "--save-wav needs a file path".to_string())?;
                    save_wav = Some(PathBuf::from(file));
                }
                _ if chd.is_none() => chd = Some(PathBuf::from(arg)),
                // A bare number after the CHD path selects the track.
                _ if track.is_none() => {
                    track = Some(
                        arg.parse()
                            .map_err(|_| format!("not a track number: {arg}"))?,
                    );
                }
                other => return Err(format!("unexpected argument: {other}")),
            }
        }

        Ok(Self {
            chd: chd.ok_or_else(|| "missing <path-to-chd>".to_string())?,
            track,
            save_wav,
        })
    }
}
