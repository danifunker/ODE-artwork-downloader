# Bump `opticaldiscs` to 0.6.0 (file metadata + Joliet/Rock Ridge)

> **Task prompt** for a Claude Code session working in this repo
> (`ODE-artwork-downloader`). Self-contained: follow it top to bottom, then run
> the verification steps before reporting done. Line numbers are approximate —
> `rg` for symbols.

## Goal

Bump the `opticaldiscs` dependency from **`0.5`** to **`0.6`**. This is expected
to be a **near-zero-diff bump**: 0.6.0's breaking change is *additive* (two new
public fields on `FileEntry`), which does not break code that only reads
`FileEntry` — which is all ODE does. The upside is better disc-name coverage:
**Joliet (Unicode/long names)** and **Rock Ridge (long names + POSIX)** now
surface automatically, which can improve token/title extraction on
Windows- and Unix-authored discs.

> ODE currently pins `opticaldiscs = "0.5"` (`^0.5` = `<0.6.0`), so the pin
> **must** be bumped for Cargo to pick up 0.6.0.

## Why bump

1. **Better names for tokenization (automatic).** The ISO 9660 browser now
   prefers a Joliet tree (UTF-16BE Unicode names) when present, and reads Rock
   Ridge/SUSP long names. Discs that previously exposed only truncated 8.3-style
   identifiers now yield full, correctly-cased names — better fuzzy-match tokens
   with no code change on ODE's side.
2. **New metadata available (optional).** `FileEntry` now carries
   `timestamps` (raw per-filesystem dates) and `posix` (mode/uid/gid). Not needed
   for artwork matching, but present if useful.

## The (non-)breaking change

`FileEntry` gained two public fields:

```rust
pub timestamps: Option<FileTimestamps>,
pub posix:      Option<PosixMetadata>,
```

Adding public fields to a non-`#[non_exhaustive]` struct is a *minor* semver
break in Rust: it only affects code that builds `FileEntry` with a struct literal
or exhaustively destructures it (`FileEntry { .. }` without a trailing `..`). ODE
does neither — it reads fields off `FileEntry` values returned by opticaldiscs'
browser — so **expect a clean compile with only the pin bump.**

`MasterDirectoryBlock` and `HfsPlusVolumeHeader` also gained date fields (same
additive rule). No function signatures changed.

## Required edits

### 1. `Cargo.toml`

```toml
opticaldiscs = { version = "0.6", features = ["toc"] }
```

> If `opticaldiscs` 0.6.0 is **not yet on crates.io**, point at the local checkout
> for testing and switch back once published:
>
> ```toml
> opticaldiscs = { path = "../opticaldiscs-rs", features = ["toc"] }
> ```

0.6.0 still uses `libchdman-rs 0.288.8` (unchanged from 0.5.0). Run
`cargo tree -i libchdman-rs` and confirm a **single** copy resolves; update ODE's
direct pin if it drifted.

### 2. Confirm nothing broke (usually nothing)

```sh
rg -n 'FileEntry\s*\{|MasterDirectoryBlock\s*\{|HfsPlusVolumeHeader\s*\{' src tests
```

If any hit constructs or exhaustively destructures one of these without a
trailing `..`, add the new fields (or `, ..`). Expect **no hits** in ODE's own code.

## Optional follow-ups (only if it measurably helps matching — don't gold-plate)

- The Joliet/Rock Ridge name improvements happen automatically. If ODE keeps
  golden/snapshot tests of extracted disc names or tokens, some may shift for
  Joliet/Rock-Ridge discs because names are now longer/Unicode — that's the
  *correct* new behavior; update the goldens and note which names changed.
- `FileEntry::posix` (with `is_symlink()`) and `symlink_target` are now populated
  for Rock Ridge/HFS discs; consider skipping symlinks/invisibles when
  tokenizing if they add noise.

## Verification (run all; report output)

```sh
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
cargo tree -i libchdman-rs   # confirm a single 0.288.8 copy
```

The previous baseline was 50 tests, 0 failed — confirm it still passes. If any
title/token golden shifts due to the Joliet/Rock-Ridge name upgrade, update it
and note the change.

## Done criteria

- `Cargo.toml` depends on `opticaldiscs = "0.6"` (crates.io) — or the path dep if
  0.6.0 isn't published yet, with a note to flip it back.
- Clean compile (no `FileEntry` literal/exhaustive-match breakage).
- All four verification commands clean and the test suite passes.
