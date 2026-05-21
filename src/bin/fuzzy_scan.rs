//! Batch fuzzy-match scanner.
//!
//! Reads disc-image paths (one per line on stdin, or as positional args), runs
//! the exact redump cascade followed by fuzzy matching on each, and writes the
//! results as CSV or JSON for review/threshold tuning.
//!
//! Examples:
//!   find /discs -name '*.cue' | fuzzy_scan --out results.csv
//!   fuzzy_scan --format json --top 10 --out results.json file1.cue file2.iso
//!
//! Flags:
//!   --out <path>       Output file (default: stdout).
//!   --format csv|json  Output format (default: inferred from --out, else csv).
//!   --top <N>          Max fuzzy candidates per file (default: 5).
//!
//! Each file is read in a child process (`--scan-one`) so a crash in the
//! native CHD/ISO reader on a malformed or non-CD image is recorded as a
//! `read_error` instead of aborting the whole batch.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use std::sync::{Arc, Mutex};

use ode_artwork_downloader::api::{search_by_discid, MusicBrainzResult};
use ode_artwork_downloader::config::get_config;
use ode_artwork_downloader::db::{
    cascade, fuzzy_from_disc, CascadeInputs, DatabaseManager, FuzzyCandidate, RedumpMatch,
    ScoreSource,
};
use ode_artwork_downloader::disc::hasher::{hash_data_track, HashProgress};
use ode_artwork_downloader::disc::{detect_sector_layout, DiscFormat, DiscReader};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Csv,
    Json,
}

struct Args {
    out: Option<PathBuf>,
    format: Format,
    top: usize,
    no_hash: bool,
    no_musicbrainz: bool,
    no_deep_fs: bool,
    paths: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut out = None;
    let mut format: Option<Format> = None;
    let mut top = 5usize;
    let mut no_hash = false;
    let mut no_musicbrainz = false;
    let mut no_deep_fs = false;
    let mut paths = Vec::new();

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" | "-o" => {
                out = Some(PathBuf::from(
                    it.next().ok_or("--out requires a path")?,
                ));
            }
            "--format" | "-f" => {
                format = Some(match it.next().as_deref() {
                    Some("csv") => Format::Csv,
                    Some("json") => Format::Json,
                    other => return Err(format!("invalid --format: {other:?}")),
                });
            }
            "--top" | "-t" => {
                top = it
                    .next()
                    .ok_or("--top requires a number")?
                    .parse()
                    .map_err(|e| format!("invalid --top: {e}"))?;
            }
            "--list" | "-l" => {
                let list_path = it.next().ok_or("--list requires a path")?;
                let content = std::fs::read_to_string(&list_path)
                    .map_err(|e| format!("could not read --list {list_path}: {e}"))?;
                for line in content.lines() {
                    let t = line.trim();
                    if !t.is_empty() {
                        paths.push(PathBuf::from(t));
                    }
                }
            }
            "--no-hash" => {
                no_hash = true;
            }
            "--no-musicbrainz" | "--no-mb" => {
                no_musicbrainz = true;
            }
            "--no-deep-filesystem-search" | "--no-deep" => {
                no_deep_fs = true;
            }
            "-h" | "--help" => {
                return Err("help".to_string());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag: {other}"));
            }
            other => paths.push(PathBuf::from(other)),
        }
    }

    // Infer format from output extension when not given explicitly.
    let format = format.unwrap_or_else(|| {
        match out.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()) {
            Some("json") => Format::Json,
            _ => Format::Csv,
        }
    });

    // No positional paths → read them from stdin (the `find | scan` case).
    if paths.is_empty() {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.map_err(|e| format!("stdin read: {e}"))?;
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                paths.push(PathBuf::from(trimmed));
            }
        }
    }

    Ok(Args { out, format, top, no_hash, no_musicbrainz, no_deep_fs, paths })
}

/// One output record per (file, hit). A file with no hits still produces one
/// record so the scan is a complete accounting of the input list.
#[derive(Serialize, Deserialize)]
struct Record {
    file: String,
    status: String, // ok | read_error
    match_type: String, // exact | fuzzy | none
    redump_id: Option<i64>,
    title: String,
    system: String,
    score: Option<f64>,
    sources: String,
    inferred_version: String,
    size_ratio: Option<f64>,
    reason: String,
    redump_url: String,
}

fn sources_str(sources: &[ScoreSource]) -> String {
    sources
        .iter()
        .map(|s| match s {
            ScoreSource::Pvd => "pvd",
            ScoreSource::Title => "title",
            ScoreSource::Tracks => "tracks",
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn exact_record(file: &str, m: &RedumpMatch) -> Record {
    Record {
        file: file.to_string(),
        status: "ok".into(),
        match_type: "exact".into(),
        redump_id: Some(m.redump_id),
        title: m.title.clone(),
        system: m.system.clone(),
        score: Some(1.0),
        sources: format!("{:?}", m.matched_via),
        inferred_version: String::new(),
        size_ratio: None,
        reason: format!("exact via {:?}", m.matched_via),
        redump_url: m.redump_url.clone(),
    }
}

fn fuzzy_record(file: &str, c: &FuzzyCandidate) -> Record {
    Record {
        file: file.to_string(),
        status: "ok".into(),
        match_type: "fuzzy".into(),
        redump_id: Some(c.redump_id),
        title: c.title.clone(),
        system: c.system.clone(),
        score: Some(c.score),
        sources: sources_str(&c.sources),
        inferred_version: c.inferred_version.clone().unwrap_or_default(),
        size_ratio: c.size_ratio,
        reason: c.match_reason.clone(),
        redump_url: c.redump_url.clone(),
    }
}

fn musicbrainz_record(file: &str, r: &MusicBrainzResult) -> Record {
    let title = if r.artist.is_empty() {
        r.title.clone()
    } else {
        format!("{} - {}", r.artist, r.title)
    };
    Record {
        file: file.to_string(),
        status: "ok".into(),
        match_type: "musicbrainz".into(),
        redump_id: None,
        title,
        system: "audio-cd".into(),
        score: Some(1.0),
        sources: "musicbrainz-discid".into(),
        inferred_version: String::new(),
        size_ratio: None,
        reason: r.date.clone().unwrap_or_default(),
        redump_url: format!("https://musicbrainz.org/release/{}", r.release_id),
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_csv(w: &mut dyn Write, records: &[Record]) -> std::io::Result<()> {
    writeln!(
        w,
        "file,status,match_type,redump_id,title,system,score,sources,inferred_version,size_ratio,reason,redump_url"
    )?;
    for r in records {
        let score = r.score.map(|s| format!("{s:.4}")).unwrap_or_default();
        let ratio = r.size_ratio.map(|s| format!("{s:.4}")).unwrap_or_default();
        let id = r.redump_id.map(|i| i.to_string()).unwrap_or_default();
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&r.file),
            r.status,
            r.match_type,
            id,
            csv_escape(&r.title),
            csv_escape(&r.system),
            score,
            csv_escape(&r.sources),
            csv_escape(&r.inferred_version),
            ratio,
            csv_escape(&r.reason),
            csv_escape(&r.redump_url),
        )?;
    }
    Ok(())
}

fn write_json(w: &mut dyn Write, records: &[Record]) -> std::io::Result<()> {
    let arr: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            serde_json::json!({
                "file": r.file,
                "status": r.status,
                "match_type": r.match_type,
                "redump_id": r.redump_id,
                "title": r.title,
                "system": r.system,
                "score": r.score,
                "sources": r.sources,
                "inferred_version": r.inferred_version,
                "size_ratio": r.size_ratio,
                "reason": r.reason,
                "redump_url": r.redump_url,
            })
        })
        .collect();
    let s = serde_json::to_string_pretty(&arr).expect("serialize");
    writeln!(w, "{s}")
}

/// Read + cascade + fuzzy for a single file. Runs inside the child process.
fn scan_one_file(
    conn: &rusqlite::Connection,
    top: usize,
    no_hash: bool,
    no_musicbrainz: bool,
    no_deep_fs: bool,
    path: &PathBuf,
) -> Vec<Record> {
    let file = path.display().to_string();
    let cfg = &get_config().fuzzy_match;
    let mut records = Vec::new();

    let info = match DiscReader::read(path) {
        Ok(info) => info,
        Err(e) => {
            records.push(Record {
                file,
                status: "read_error".into(),
                match_type: "none".into(),
                redump_id: None,
                title: String::new(),
                system: String::new(),
                score: None,
                sources: String::new(),
                inferred_version: String::new(),
                size_ratio: None,
                reason: e.to_string(),
                redump_url: String::new(),
            });
            return records;
        }
    };

    // MusicBrainz tier — runs first for any disc with an audio TOC. Uses the
    // EXACT MusicBrainz disc-ID (a hash of the full track layout), not the
    // fuzzy TOC fallback, so a mixed-mode game disc can't be misclassified as
    // a soundtrack: it would only match if that exact disc were submitted to
    // MB. Network call; disable with --no-musicbrainz.
    if !no_musicbrainz {
        if let Some(toc) = info.toc.as_ref() {
            let disc_id = toc.musicbrainz_id();
            match search_by_discid(&disc_id, None) {
                Ok(results) if !results.is_empty() => {
                    for r in &results {
                        records.push(musicbrainz_record(&file, r));
                    }
                    return records;
                }
                Ok(_) => {}
                Err(e) => eprintln!("  musicbrainz error: {e}"),
            }
        }
    }

    // Cheap exact lookup first: serial / PVD volume id. These read filesystem
    // metadata and cost nothing, so try them before any hashing.
    let serial = info.parsed_filename.serial.as_deref();
    let pvd_volume_id = info
        .pvd
        .as_ref()
        .map(|p| p.volume_id.trim())
        .filter(|s| !s.is_empty());
    let cheap_inputs = CascadeInputs {
        serial,
        pvd_volume_id,
        pvd_creation_date: None,
        ..Default::default()
    };
    match cascade(conn, &cheap_inputs) {
        Ok(Some(exact)) if !exact.is_empty() => {
            for m in &exact {
                records.push(exact_record(&file, m));
            }
            return records;
        }
        Ok(_) => {}
        Err(e) => eprintln!("  cascade error: {e}"),
    }

    // Hash-based exact match — only when it can actually pay off. Redump hashes
    // raw 2352-byte sectors. bin/cue and chd preserve raw sectors, so they're
    // always worth hashing. A bare ISO is the ambiguous case: cooked (2048) can
    // never match, so probe the sector layout and skip the slow hash unless it's
    // a rare raw ISO.
    if !no_hash {
        let hashable = match info.format {
            DiscFormat::BinCue | DiscFormat::Chd => true,
            DiscFormat::Iso => detect_sector_layout(path).is_hashable_for_redump(),
            _ => true, // let the hasher decide / fail gracefully
        };
        if hashable {
            let progress = Arc::new(Mutex::new(HashProgress::default()));
            match hash_data_track(&info, progress) {
                Ok(h) => {
                    let inputs = CascadeInputs {
                        track_sha1: Some(h.sha1.as_str()),
                        track_md5: Some(h.md5.as_str()),
                        track_crc32: Some(h.crc32.as_str()),
                        ..Default::default()
                    };
                    match cascade(conn, &inputs) {
                        Ok(Some(exact)) if !exact.is_empty() => {
                            for m in &exact {
                                records.push(exact_record(&file, m));
                            }
                            return records;
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("  hash cascade error: {e}"),
                    }
                }
                Err(e) => eprintln!("  hashing skipped: {e}"),
            }
        } else {
            eprintln!("  hashing skipped: cooked/unknown sector layout (can't match redump)");
        }
    }

    // Fuzzy fallback. `!no_deep_fs` controls whether `fuzzy_from_disc` may
    // walk the disc filesystem to enrich title candidates and verify hits.
    match fuzzy_from_disc(conn, &info, cfg, !no_deep_fs) {
        Ok(candidates) if !candidates.is_empty() => {
            for c in candidates.iter().take(top) {
                records.push(fuzzy_record(&file, c));
            }
        }
        Ok(_) => records.push(Record {
            file,
            status: "ok".into(),
            match_type: "none".into(),
            redump_id: None,
            title: info.title.clone(),
            system: String::new(),
            score: None,
            sources: String::new(),
            inferred_version: String::new(),
            size_ratio: None,
            reason: "no candidates above floor".to_string(),
            redump_url: String::new(),
        }),
        Err(e) => eprintln!("  fuzzy error: {e}"),
    }
    records
}

fn open_db() -> Result<rusqlite::Connection, ExitCode> {
    match DatabaseManager::new().and_then(|m| m.open()) {
        Ok(conn) => Ok(conn),
        Err(e) => {
            eprintln!("could not open redump DB: {e}\n(run the GUI once to download it)");
            Err(ExitCode::FAILURE)
        }
    }
}

/// Child mode: `--scan-one <path> [--top N]`. Scans one file and prints its
/// records as a JSON array to stdout. Isolated so a native-reader crash on a
/// bad image doesn't take down the parent batch.
fn run_scan_one(mut it: impl Iterator<Item = String>) -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut top = 5usize;
    let mut no_hash = false;
    let mut no_musicbrainz = false;
    let mut no_deep_fs = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--top" => top = it.next().and_then(|s| s.parse().ok()).unwrap_or(5),
            "--no-hash" => no_hash = true,
            "--no-musicbrainz" | "--no-mb" => no_musicbrainz = true,
            "--no-deep-filesystem-search" | "--no-deep" => no_deep_fs = true,
            other => path = Some(PathBuf::from(other)),
        }
    }
    let Some(path) = path else {
        eprintln!("--scan-one requires a path");
        return ExitCode::FAILURE;
    };
    let conn = match open_db() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let records = scan_one_file(&conn, top, no_hash, no_musicbrainz, no_deep_fs, &path);
    let json = serde_json::to_string(&records).expect("serialize records");
    println!("{json}");
    ExitCode::SUCCESS
}

/// Scan one file in a child process; on crash/non-zero exit, synthesize a
/// `read_error` record so the batch keeps going.
fn scan_one_isolated(
    exe: &std::path::Path,
    path: &PathBuf,
    top: usize,
    no_hash: bool,
    no_musicbrainz: bool,
    no_deep_fs: bool,
) -> Vec<Record> {
    let file = path.display().to_string();
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--scan-one")
        .arg(path)
        .arg("--top")
        .arg(top.to_string());
    if no_hash {
        cmd.arg("--no-hash");
    }
    if no_musicbrainz {
        cmd.arg("--no-musicbrainz");
    }
    if no_deep_fs {
        cmd.arg("--no-deep-filesystem-search");
    }
    let output = cmd.output();
    match output {
        Ok(out) if out.status.success() => {
            serde_json::from_slice::<Vec<Record>>(&out.stdout).unwrap_or_else(|e| {
                vec![read_error_record(&file, format!("output parse error: {e}"))]
            })
        }
        Ok(out) => vec![read_error_record(
            &file,
            format!("reader crashed or failed ({})", out.status),
        )],
        Err(e) => vec![read_error_record(&file, format!("spawn error: {e}"))],
    }
}

fn read_error_record(file: &str, reason: String) -> Record {
    Record {
        file: file.to_string(),
        status: "read_error".into(),
        match_type: "none".into(),
        redump_id: None,
        title: String::new(),
        system: String::new(),
        score: None,
        sources: String::new(),
        inferred_version: String::new(),
        size_ratio: None,
        reason,
        redump_url: String::new(),
    }
}

fn main() -> ExitCode {
    // Child mode short-circuits before normal arg parsing / stdin reading.
    let mut raw = std::env::args().skip(1);
    if let Some(first) = raw.next() {
        if first == "--scan-one" {
            return run_scan_one(raw);
        }
    }

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if e == "help" {
                eprintln!(
                    "Usage: fuzzy_scan [--out FILE] [--format csv|json] [--top N] [--list FILE]\n\
                     \x20              [--no-hash] [--no-musicbrainz] [--no-deep-filesystem-search] [PATHS...]\n\
                     Reads paths from --list FILE, positional args, or stdin (in that order of availability).\n\
                     Hashing runs only on raw (2352-byte) images; cooked ISOs skip it automatically.\n\
                     Order: MusicBrainz (audio TOC) -> exact (serial/PVD/hash) -> fuzzy -> deep filesystem dig.\n\
                     --no-hash disables hashing; --no-musicbrainz (--no-mb) disables the MB network lookup;\n\
                     --no-deep-filesystem-search (--no-deep) skips the fuzzy stage's disc walk (faster,\n\
                     but won't enrich titles from on-disc filenames or verify candidates against contents)."
                );
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if args.paths.is_empty() {
        eprintln!("no input paths (pass as args or pipe via stdin)");
        return ExitCode::FAILURE;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("could not resolve own path for child scans: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut records: Vec<Record> = Vec::new();
    let total = args.paths.len();

    for (i, path) in args.paths.iter().enumerate() {
        eprintln!("[{}/{}] {}", i + 1, total, path.display());
        records.extend(scan_one_isolated(
            &exe,
            path,
            args.top,
            args.no_hash,
            args.no_musicbrainz,
            args.no_deep_fs,
        ));
    }

    let write_result = match args.out {
        Some(ref p) => match std::fs::File::create(p) {
            Ok(f) => {
                let mut w = std::io::BufWriter::new(f);
                let r = match args.format {
                    Format::Csv => write_csv(&mut w, &records),
                    Format::Json => write_json(&mut w, &records),
                };
                r.and_then(|_| w.flush())
            }
            Err(e) => {
                eprintln!("could not create {}: {e}", p.display());
                return ExitCode::FAILURE;
            }
        },
        None => {
            let stdout = std::io::stdout();
            let mut w = stdout.lock();
            match args.format {
                Format::Csv => write_csv(&mut w, &records),
                Format::Json => write_json(&mut w, &records),
            }
        }
    };

    if let Err(e) = write_result {
        eprintln!("write error: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "done: {} input(s), {} record(s){}",
        total,
        records.len(),
        args.out
            .as_ref()
            .map(|p| format!(" → {}", p.display()))
            .unwrap_or_default()
    );
    ExitCode::SUCCESS
}
