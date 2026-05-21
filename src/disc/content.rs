//! Disc filesystem inspection — phrase candidates + flat token set.
//!
//! Walks the disc filesystem once, harvesting two views of the content:
//!
//! - **`phrase_candidates`** — distinctive directory names, executable
//!   basenames, autorun.inf labels, and first lines of readmes. These are
//!   fed into fuzzy candidate generation as title-like strings (e.g.
//!   `\TOMBRAID\` → `TOMBRAID` → matches "Tomb Raider" via acronym/token).
//! - **`tokens`** — the flat lowercase token bag used by the verifier to
//!   corroborate or contradict a fuzzy candidate's distinctive title words.
//!
//! Bounded by a byte budget so we never read the whole disc. Identity-style
//! text files (`*.txt`/`*.nfo`/`autorun.inf`/`README*`) are read in priority
//! order until the budget is spent.

use std::collections::HashSet;

use crate::disc::browse::{open_filesystem, EntryType, FileEntry, Filesystem};
use crate::disc::DiscInfo;

/// Total bytes we're willing to read off the disc during a single pass.
const DEFAULT_BYTE_BUDGET: u64 = 32 * 1024 * 1024;
/// Maximum total filesystem entries enumerated. Safety net.
const MAX_ENTRIES: usize = 20_000;
/// Maximum recursion depth.
const MAX_DEPTH: usize = 12;
/// Min token length for the verifier's flat token set.
const MIN_TOKEN_LEN: usize = 3;
/// Minimum length of a directory/exe name to consider as a phrase candidate.
const MIN_PHRASE_LEN: usize = 3;
/// Cap on phrase candidates to avoid pathological libraries.
const MAX_PHRASE_CANDIDATES: usize = 24;

const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "nfo", "inf", "ini", "rtf", "doc", "diz", "md", "readme", "1st",
    "log", "html", "htm", "xml", "url",
];

const IDENTITY_FILENAMES: &[&str] = &[
    "readme", "read.me", "read_me", "readme.txt", "readme.1st",
    "autorun.inf", "setup.inf", "version", "version.txt",
];

/// Directory and executable basenames so generic they carry no identity.
/// Anything matching these (case-insensitive) is skipped as a phrase
/// candidate (but its tokens still go into the verifier's flat set).
const GENERIC_PHRASES: &[&str] = &[
    "setup", "install", "installer", "uninst", "uninstall", "autorun", "readme",
    "data", "data1", "data2", "data3", "common", "shared", "system", "system32",
    "windows", "winnt", "winapps", "win", "win9x", "win98", "winxp", "macos",
    "drivers", "driver", "support", "tools", "tool", "patches", "patch",
    "extras", "extra", "bonus", "demos", "demo", "misc", "other", "files",
    "fonts", "music", "audio", "video", "movies", "movie", "images", "pics",
    "graphics", "art", "bin", "lib", "doc", "docs", "manual", "manuals",
    "help", "license", "licence", "cabinet", "cab", "src", "source", "samples",
    "sample", "examples", "example", "redist", "redistributable", "runtime",
    "msdownload", "program", "programs", "tmp", "temp", "backup", "old",
    "new", "test", "tests", "icon", "icons", "image", "trash", "info",
    "config", "configs", "settings", "options", "log", "logs", "history",
    "fmv", "movie", "speech", "sfx", "voice", "mac", "pc", "goodies",
    "addons", "addon", "add", "ons", "utilities", "utility", "applications",
    "application", "app", "apps", "contents", "resources", "library",
    // language folder names — common on multi-region discs, never a title
    "english", "french", "german", "spanish", "italian", "dutch", "japanese",
    "deutsch", "francais", "espanol", "italiano", "portugues", "svenska",
    "lang", "language", "languages", "intl", "international", "locale",
];

#[derive(Debug, Default)]
pub struct DiscContent {
    /// Distinct title-like strings extracted from the disc. Ordered roughly
    /// by signal strength: volume name, autorun/readme labels, then dir/exe
    /// basenames.
    pub phrase_candidates: Vec<String>,
    /// Flat lowercase alphanumeric token set for verifier comparisons.
    pub tokens: HashSet<String>,
    /// ISO-8601 PVD creation date (matching redump's format) if available.
    pub creation_date: Option<String>,
    /// Number of filesystem entries visited.
    pub files_seen: usize,
    /// Bytes read from text-style files during this pass.
    pub bytes_read: u64,
}

impl DiscContent {
    /// True when there's enough token signal that absence of a candidate's
    /// distinctive words can be treated as contradiction rather than
    /// abstention.
    pub fn usable_identity(&self) -> bool {
        self.tokens.len() >= 5
    }
}

/// Walk the disc and return phrase candidates + a flat token set.
/// Bounded; never panics; degrades gracefully on read errors.
pub fn read_content(info: &DiscInfo) -> DiscContent {
    let mut c = DiscContent::default();

    if let Some(pvd) = info.pvd.as_ref() {
        if let Some(d) = pvd.creation_date.as_ref() {
            c.creation_date = Some(d.to_iso8601());
        }
    }

    // Volume label seeds both views.
    if let Some(label) = info.volume_label.as_deref() {
        push_tokens(&mut c.tokens, label);
        push_phrase(&mut c.phrase_candidates, label);
    }
    // Filename-derived tokens land in the verifier's set so opaque data discs
    // (Mission Critical Disc 2: only mc001..mc500 files) still corroborate
    // from the filename's "Mission Critical" alone.
    push_filename_tokens(&mut c.tokens, &info.parsed_filename.title);
    push_filename_tokens(&mut c.tokens, &info.parsed_filename.original);

    let mut fs = match open_filesystem(info) {
        Ok(f) => f,
        Err(_) => return c,
    };
    let root = match fs.root() {
        Ok(r) => r,
        Err(_) => return c,
    };

    let mut to_read: Vec<FileEntry> = Vec::new();
    walk(&mut *fs, &root, 0, &mut c, &mut to_read);

    // Spend the byte budget on identity-style files (readmes, autorun, etc.),
    // smallest-first so the file count is maximized within the budget.
    to_read.sort_by_key(|e| (file_priority(&e.name), e.size));
    for entry in to_read {
        if c.bytes_read >= DEFAULT_BYTE_BUDGET {
            break;
        }
        let remaining = (DEFAULT_BYTE_BUDGET - c.bytes_read) as usize;
        let take = (entry.size as usize).min(remaining);
        let bytes = match fs.read_file_range(&entry, 0, take) {
            Ok(b) => b,
            Err(_) => continue,
        };
        c.bytes_read += bytes.len() as u64;

        let text: String = match std::str::from_utf8(&bytes) {
            Ok(s) => s.to_string(),
            Err(_) => bytes.iter().take(128 * 1024).map(|&b| b as char).collect(),
        };
        push_tokens(&mut c.tokens, &text);

        // autorun.inf and version files often spell out a clean title — pull
        // out high-signal lines as phrase candidates.
        let name_lc = entry.name.to_ascii_lowercase();
        if name_lc == "autorun.inf" || name_lc.starts_with("setup.inf") {
            for value in inf_label_values(&text) {
                push_phrase(&mut c.phrase_candidates, &value);
            }
        }
        if name_lc.starts_with("readme") || name_lc == "version.txt" {
            if let Some(first) = first_meaningful_line(&text) {
                push_phrase(&mut c.phrase_candidates, &first);
            }
        }
    }

    if c.phrase_candidates.len() > MAX_PHRASE_CANDIDATES {
        c.phrase_candidates.truncate(MAX_PHRASE_CANDIDATES);
    }
    c
}

fn walk(
    fs: &mut dyn Filesystem,
    dir: &FileEntry,
    depth: usize,
    c: &mut DiscContent,
    to_read: &mut Vec<FileEntry>,
) {
    if depth > MAX_DEPTH || c.files_seen >= MAX_ENTRIES {
        return;
    }
    let children = match fs.list_directory(dir) {
        Ok(ch) => ch,
        Err(_) => return,
    };
    for child in children {
        if c.files_seen >= MAX_ENTRIES {
            return;
        }
        c.files_seen += 1;
        push_tokens(&mut c.tokens, &child.name);
        if let Some(tc) = child.type_code.as_deref() {
            push_tokens(&mut c.tokens, tc);
        }
        if let Some(cc) = child.creator_code.as_deref() {
            push_tokens(&mut c.tokens, cc);
        }

        // NOTE: directory names and executable stems are intentionally NOT
        // used as phrase candidates. They proved too noisy — incidental
        // content (a bundled `Bugdom` demo, an `English` language folder, a
        // `pebuilder` stem read as the acronym "rds") generated confident but
        // wrong matches. Only readme/autorun *label lines* (read below) are
        // trusted as title declarations. Directory/exe names still feed the
        // flat token set for verification.
        match child.entry_type {
            EntryType::Directory => {
                walk(fs, &child, depth + 1, c, to_read);
            }
            EntryType::File => {
                if looks_like_identity_file(&child.name) && child.size > 0 {
                    to_read.push(child.clone());
                }
            }
        }
    }
}

fn is_generic_phrase(s: &str) -> bool {
    let lc = s.to_ascii_lowercase();
    GENERIC_PHRASES.iter().any(|g| *g == lc)
}

fn looks_like_identity_file(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if IDENTITY_FILENAMES.iter().any(|f| *f == n) {
        return true;
    }
    if let Some(stem) = std::path::Path::new(&n).file_stem().and_then(|s| s.to_str()) {
        if stem.starts_with("readme") || stem.starts_with("read_me") {
            return true;
        }
    }
    let ext = std::path::Path::new(&n)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    TEXT_EXTENSIONS.iter().any(|e| *e == ext)
}

fn file_priority(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    if IDENTITY_FILENAMES.iter().any(|f| *f == n) {
        return 0;
    }
    let ext = std::path::Path::new(&n)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "txt" | "nfo" | "inf" | "diz" | "1st" => 1,
        "md" | "rtf" | "doc" | "ini" => 2,
        "html" | "htm" | "xml" => 3,
        _ => 5,
    }
}

/// Extract `label=` (and `name=`) values from an INF-style file body.
fn inf_label_values(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let lc = line.to_ascii_lowercase();
        for key in ["label=", "name=", "appname=", "product="] {
            if let Some(idx) = lc.find(key) {
                let value = line[idx + key.len()..]
                    .trim_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',');
                if !value.is_empty() && value.len() < 80 {
                    out.push(value.to_string());
                }
            }
        }
    }
    out
}

/// First non-empty, non-trivial line of a text body, capped at 100 chars.
fn first_meaningful_line(text: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if t.len() < 5 || t.len() > 100 {
            continue;
        }
        // Skip headers / decorations.
        if t.chars().all(|c| !c.is_alphanumeric()) {
            continue;
        }
        return Some(t.to_string());
    }
    None
}

fn push_phrase(out: &mut Vec<String>, s: &str) {
    let t = s.trim();
    if t.is_empty() || t.len() < MIN_PHRASE_LEN {
        return;
    }
    // Reject single generic words ("English", "Software", "Setup"). A
    // multi-word phrase that merely contains a generic word is fine.
    if is_generic_phrase(t) {
        return;
    }
    if !out.iter().any(|e| e.eq_ignore_ascii_case(t)) {
        out.push(t.to_string());
    }
}

/// Insert a space whenever a letter is adjacent to a digit (`QUAKE106` →
/// `QUAKE 106`). Pure-letter and pure-digit strings are unchanged.
pub fn letter_digit_split(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        if let Some(p) = prev {
            let boundary = (p.is_ascii_alphabetic() && ch.is_ascii_digit())
                || (p.is_ascii_digit() && ch.is_ascii_alphabetic());
            if boundary {
                out.push(' ');
            }
        }
        out.push(ch);
        prev = Some(ch);
    }
    out
}

/// CamelCase splitter (standard two-rule pattern):
/// 1. lowercase→uppercase boundary always starts a new word;
/// 2. inside an uppercase run, the last uppercase before a lowercase starts a
///    new word (`GLQuake` → `GL Quake`).
/// `Discworld` is unchanged (one capital, no boundary).
pub fn camel_split(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(chars.len() + 4);
    for i in 0..chars.len() {
        if i > 0 {
            let prev = chars[i - 1];
            let cur = chars[i];
            let next = chars.get(i + 1).copied();
            let lower_to_upper = prev.is_ascii_lowercase() && cur.is_ascii_uppercase();
            let upper_run_to_camel = prev.is_ascii_uppercase()
                && cur.is_ascii_uppercase()
                && next.is_some_and(|c| c.is_ascii_lowercase());
            if lower_to_upper || upper_run_to_camel {
                out.push(' ');
            }
        }
        out.push(chars[i]);
    }
    out
}

/// Tokenize a filename-like string into distinctive tokens, applying camel
/// and letter-digit splits first so `MortalKombat3` yields `mortal`,
/// `kombat`, `3`. Inserts into the given set.
fn push_filename_tokens(set: &mut HashSet<String>, s: &str) {
    push_tokens(set, s);
    let split = letter_digit_split(&camel_split(s));
    if split != s {
        push_tokens(set, &split);
    }
}

/// Public tokenizer with the same rules used to build the disc-evidence set:
/// alphanumeric splits, lowercase, ≥3 chars, stopwords dropped. Used by the
/// verifier so candidate-title tokens are comparable to evidence tokens.
pub fn distinctive_tokens(text: &str) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();
    push_tokens(&mut set, text);
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

fn push_tokens(set: &mut HashSet<String>, text: &str) {
    let mut current = String::new();
    let flush = |cur: &mut String, set: &mut HashSet<String>| {
        if cur.len() >= MIN_TOKEN_LEN && !is_stopword(cur) {
            set.insert(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else {
            flush(&mut current, set);
        }
    }
    flush(&mut current, set);
}

fn is_stopword(s: &str) -> bool {
    matches!(
        s,
        "the" | "and" | "for" | "with" | "this" | "that" | "from" | "you" | "your"
        | "are" | "was" | "all" | "any" | "but" | "not" | "can" | "has" | "have"
        | "will" | "use" | "new" | "old" | "data" | "file" | "files"
        | "copy" | "right" | "rights" | "reserved" | "version" | "ver" | "vol"
        | "volume" | "disk" | "disc" | "rom" | "cdrom" | "cdr" | "win" | "windows"
        | "mac" | "macintosh" | "system" | "setup" | "install" | "user" | "info"
        | "readme" | "txt" | "exe" | "dll" | "com" | "bat" | "ini" | "cfg" | "log"
        | "tmp" | "bin" | "lib" | "etc" | "src" | "doc" | "docs" | "html" | "htm"
        | "see" | "page" | "url" | "www" | "http" | "https"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_filter_excludes_obvious_junk() {
        assert!(is_generic_phrase("Setup"));
        assert!(is_generic_phrase("DRIVERS"));
        assert!(is_generic_phrase("data1"));
        assert!(!is_generic_phrase("TOMBRAID"));
        assert!(!is_generic_phrase("Sierra"));
    }

    #[test]
    fn push_phrase_rejects_generic_singles() {
        let mut v = Vec::new();
        push_phrase(&mut v, "English");
        push_phrase(&mut v, "Setup");
        push_phrase(&mut v, "Star Trek Generations");
        assert_eq!(v, vec!["Star Trek Generations".to_string()]);
    }

    #[test]
    fn inf_label_parsing() {
        let body = r#"[autorun]
OPEN=setup.exe
icon=setup.exe,0
label=Quake II
"#;
        let values = inf_label_values(body);
        assert!(values.iter().any(|v| v == "Quake II"));
    }

    #[test]
    fn first_line_skips_decoration() {
        let body = "================\nQuake II - Readme\n----------------\n";
        assert_eq!(first_meaningful_line(body).as_deref(), Some("Quake II - Readme"));
    }
}
