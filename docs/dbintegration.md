# DB Integration — App-side Plumbing

Follow-up work for `ODE-artwork-downloader` once the `ODE-lookup-db` repo is producing data. The DB repo is the source of truth for redump-derived disc metadata; this app consumes it to identify discs deterministically before falling back to the existing fuzzy search.

## Prerequisites

- `ODE-lookup-db` is publishing a `redump.sqlite` artifact (committed to the repo or attached as a release asset — TBD when the DB repo is built).
- Schema documented in that repo's `schema/disc.schema.json`. Schema version is embedded in the SQLite file.

## Work items

### 1. DB fetcher

- On first launch and periodically thereafter, download the latest `redump.sqlite` from the DB repo.
- Cache in the user config dir (platform-appropriate: `~/Library/Application Support/...` on macOS, `%APPDATA%` on Windows, `~/.config/...` on Linux).
- Honor ETag / `If-Modified-Since` so re-checks are cheap.
- Fail soft: if the download fails, fall back to whatever local copy exists; if none exists, skip DB lookups entirely and use the existing flow.

### 2. Lookup cascade

Wire into the existing disc-identification path. Order matters — earlier steps are more deterministic.

1. Compute track hashes (CRC32 / MD5 / SHA-1) from the loaded disc image.
2. Query SQLite by hash. If hit, use that metadata and skip fuzzy search entirely.
3. Else query by serial / barcode if extractable from the disc.
4. Else query by PVD signature (`volume_identifier` + `system_identifier` + `creation_date`).
5. Else fall through to the existing DuckDuckGo / filename flow.

### 3. Schema version check

- Read the embedded `schema_version` from the SQLite file at load time.
- If newer than the app supports, warn the user (DB may contain fields this app version doesn't understand) but continue with best-effort reads.
- If older than expected, treat as fine — readers ignore missing optional fields.

### 4. Local miss logging

- When the cascade produces no match, append the disc's identifying info (hashes, PVD, filename) to a local log file in the user config dir.
- Purpose: let users later contribute these back to redump manually. No telemetry, no network calls.

### 5. Audio-CD routing unchanged

Audio-only discs continue to route to MusicBrainz. Mixed-mode discs (data track + audio tracks) try redump first, MusicBrainz second.

## Non-goals

- No write-back to the DB repo from the app.
- No bundled SQLite shipped inside the app binary — always fetched at runtime.
- No background sync daemon — update on launch (and optionally on a manual "refresh DB" button).

## Open questions

- Exact URL pattern for fetching the latest SQLite (depends on whether DB repo commits the file or uses release assets).
- Whether to expose a "force refresh DB" action in the UI or rely on periodic auto-check.
- Where the miss log lives and whether to surface it in the UI.
