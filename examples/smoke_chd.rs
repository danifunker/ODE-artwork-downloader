//! Downstream smoke test for the opticaldiscs 0.4.0 / libchdman-rs swap.
//!
//! Usage:
//!   cargo run --release --example smoke_chd -- <path-to-chd>
//!
//! Reads the CHD through opticaldiscs the same way the main app does, then
//! prints a small report so we can sanity-check that:
//!   - the CHD opens
//!   - track metadata parses (multi-track is interesting)
//!   - the data track yields a real PVD / volume label
//!   - if there are audio tracks, the TOC is populated for MusicBrainz
//!
//! No claims about hash correctness — this is just "is the read path alive
//! after the swap?"

use std::path::PathBuf;

use ode_artwork_downloader::disc::DiscReader;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: smoke_chd <path-to-chd>");

    if !path.exists() {
        eprintln!("file does not exist: {}", path.display());
        std::process::exit(2);
    }

    println!("Reading: {}", path.display());
    let info = match DiscReader::read(&path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("DiscReader::read failed: {e}");
            std::process::exit(1);
        }
    };

    println!("Format:        {:?}", info.format);
    println!("Filesystem:    {:?}", info.filesystem);
    println!(
        "Volume label:  {}",
        info.volume_label.as_deref().unwrap_or("(none)")
    );
    println!("Title guess:   {}", info.title);
    println!("Confidence:    {:?}", info.confidence);

    if let Some(pvd) = &info.pvd {
        println!("PVD vol-id:    {}", pvd.volume_id.trim());
        println!("PVD sys-id:    {}", pvd.system_id.trim());
        println!("PVD publisher: {}", pvd.publisher_id.trim());
    } else {
        println!("PVD:           (none — not an ISO9660 disc?)");
    }

    if let Some(toc) = &info.toc {
        println!(
            "TOC tracks:    {} (first={}, last={})",
            toc.track_count(),
            toc.first_track,
            toc.last_track
        );
        println!("MB disc ID:    {}", toc.musicbrainz_id());
    } else {
        println!("TOC:           (none)");
    }

    if let Some(serial) = &info.parsed_filename.serial {
        println!("Filename serial: {serial}");
    }

    println!("\nSmoke test OK.");
}
