# Fuzzy Match Against Redump

Extension to the lookup cascade in [`dbintegration.md`](dbintegration.md). When a disc doesn't match any redump row exactly, return a ranked list of *possible matches* with confidence scores instead of failing through to the existing DDG/filename flow.

The initial release is intentionally **loose** — we want to see a lot of candidates per disc so we can judge how well the scoring tracks reality. Thresholds tighten once we have data.

## Goals

- Identify discs that aren't bit-exact in redump (re-burns, region variants, no-CD patches, mastering typos) by their PVD signature and on-disc title text.
- Always return ranked candidates with a confidence score, not a single answer. The caller decides what to do with the list.
- Keep the lookup-db schema unchanged. All fuzzy logic lives app-side.

## Non-goals

- No **content** fingerprinting (hashing the full file tree of the disc). We do peek at directory listings to extract title hints and total payload size — that's metadata, not a fingerprint.
- No perceptual hashing of on-disc imagery.
- No publisher-name extraction in the initial release (deferred — would need a publisher dictionary and a reliable extraction source; revisit if Sources A–D underperform).
- No write-back of fuzzy matches to the lookup-db.
- No AI/LLM scoring in the match itself (vision-model sanity checks are a separate concern in the artwork-repository plan).

## Cascade placement

Slots in between steps 4 and 5 of the existing cascade in `dbintegration.md`:

1. Track-hash exact match.
2. Serial / barcode exact match.
3. PVD-signature exact match.
4. **(new)** Fuzzy match — returns ranked candidate list.
5. Existing DDG / filename fallback (only if fuzzy returns nothing above the floor threshold).

## Match sources

Four independent scorers run in parallel; results are merged at the end.

### Source A — PVD relaxed match

Run three relaxation queries against the redump DB, each holding two of the three PVD fields exact and fuzzing the third:

| Query | Exact fields                                | Fuzzed field          | Match rule                          |
| ----- | ------------------------------------------- | --------------------- | ----------------------------------- |
| A1    | `system_identifier`, `creation_date`        | `volume_identifier`   | Levenshtein ratio ≥ 0.70            |
| A2    | `volume_identifier`, `system_identifier`    | `creation_date`       | within ±30 days                     |
| A3    | `volume_identifier`, `creation_date`        | `system_identifier`   | Levenshtein ratio ≥ 0.70            |

Volume-identifier comparison must first **strip trailing version suffixes** (e.g. `QUAKE_106` → `QUAKE`, `DOOM2_19` → `DOOM2`) — the trailing `_NNN` / `_VNN` / `-VN_NN` patterns encode the patch level and shouldn't penalize matching the underlying title. Keep the raw value in the candidate record so the version is recoverable for inference (see [Volume-label inference](#volume-label-inference) below).

Per-candidate score: 1.0 for each exact field + the fuzzed field's similarity (ratio for A1/A3; `1 - abs(days_diff)/30` for A2), then divided by 3 to land in [0, 1].

The 0.70 ratio and ±30-day window are deliberately wide for the initial release. Tighten once we see real candidate distributions.

### Source B — Title fuzzy match (with abbreviation handling)

Extract title candidates from the disc in priority order:

1. `volume_identifier`, normalized (lowercase, underscores → spaces, collapse whitespace, strip version suffixes as in Source A, strip trailing volume numbers like ` DISC 1`).
2. `autorun.inf` `label=` value if present in the root of any data track.
3. First non-empty line of any root-level `README*` / `READ.ME` / `READ_ME*` file.
4. **Distinctive filename / directory stems** from the data track root and one level deep — executable basenames (`SQ6.EXE`, `SSF2T.EXE`), install-dir names (`\QUAKE\`, `\SIERRA\SQ5\`), and CD-label files (`SQV_CD`, `KQ6CD`). Skip generic stems (`setup`, `install`, `readme`, `autorun`, `data`, `cd1`, single letters).

For each title candidate, compute scores against redump titles via three matchers and take the **max**:

- **Token-set ratio** on the candidate vs. the redump `title` (handles word reordering and subtitles).
- **Acronym match**: build an acronym from the redump title's significant words (first letter of each, plus any embedded digits/roman numerals → arabic), e.g. `Super Street Fighter II Turbo` → `ssf2t`, `Space Quest V` → `sq5`. Compare to the candidate as a string-equality / startswith check. Roman numerals (I, II, III, IV, V, VI, VII, VIII, IX, X) are normalized to digits on both sides before comparison.
- **Substring containment**: if a normalized candidate appears as a substring of the normalized title (or vice versa) and is ≥ 3 chars, score = `len(shorter) / len(longer)`.

Initial threshold for inclusion: best matcher score ≥ 0.70 (or an exact acronym hit, which counts as 1.0).

Per-candidate score: the best matcher score across all title candidates, with the matcher name recorded in `match_reason`.

### Source C — Track-signature match

Many PC/Mac CDs are mixed-mode (data track + audio tracks) and the **track layout is a strong fingerprint**. A bonus edition with one extra audio track, or a re-release with re-encoded audio, will diverge from the canonical SHA-1 but keep a near-identical track structure.

Inputs from the disc image: ordered list of `(track_type, duration_sectors)` where `track_type ∈ {data, audio}`. Redump rows already carry this in their track listing.

Scoring:

1. Find redump rows whose **track count differs by ≤ 1** from the disc.
2. For each, align tracks left-to-right; allow a single insertion or deletion to account for a bonus track. For each aligned track:
   - same type required (data vs audio mismatch → reject this candidate),
   - duration must be within **±150 sectors (~2 seconds)** to count as a track match.
3. Candidate score = `matched_tracks / max(disc_tracks, candidate_tracks)`. A perfect-length match with one extra track scores `N / (N+1)`.

Minimum score to be considered a candidate: ≥ 0.70.

This source is intentionally tolerant on track count but strict on per-track duration — the goal is to catch bonus/limited editions and minor remasters without firing on entirely different games that happen to have a similar runtime.

### Source D — Payload-size sanity (modifier, not a generator)

Source D does not generate candidates. It runs over the union of A+B+C candidates and applies a **size sanity penalty**:

- Disc-side input: total bytes of data-track payload (sum of file sizes in the ISO, or total data-track byte count).
- Compare to the candidate's redump total data size. Let `r = min(disc, candidate) / max(disc, candidate)`.
- If `r ≥ 0.80` → no penalty.
- If `0.50 ≤ r < 0.80` → multiply candidate score by 0.85.
- If `r < 0.50` → drop the candidate entirely. A volume label that matches a disc with 1/10 the payload is not the same game.

This catches the QUAKE_106 vs. a tenth-of-the-size shovelware-disc-with-QUAKE-in-the-label case directly.

## Volume-label inference

When a candidate's volume label parses as `<title-stem>_<version>` (e.g. `QUAKE_106` → stem `QUAKE`, version `1.06`) **and** that stem fuzzy-matches the candidate's redump title, attach an `inferred_version` field to the candidate. Doesn't affect the score — purely metadata for the caller, useful when the disc is otherwise unidentifiable but the label spells out what it is.

## Merging and ranking

- Take the union of candidates from A, B, and C, keyed by `redump_id`.
- Final score = `max(score_A, score_B, score_C)`, plus a **+0.05 agreement bonus per additional source** that also produced this candidate (capped at +0.10, then at 1.0 overall). Two-source agreement = +0.05; three-source agreement = +0.10.
- Apply Source D's payload-size modifier to each surviving candidate.
- Sort descending by final score.
- Return the top **N = 20** candidates above a **floor of 0.60** (intentionally generous for the initial release).

Each returned candidate carries: `redump_id`, `title`, `system`, `score`, `score_sources` (set of `pvd` | `title` | `tracks`), `size_ratio` (from Source D), optional `inferred_version`, and a short `match_reason` string for logging.

## Caller contract

The fuzzy module returns a `Vec<FuzzyCandidate>` (possibly empty). It never picks a single winner. The caller — the artwork downloader's identification path — decides whether to:

- Auto-apply the top candidate (only if there's a future, higher T_auto threshold we settle on after data review),
- Show the list to the user,
- Or treat as no-match and fall through.

For the initial release, the planned behavior is: **always show the ranked list in the UI**, regardless of score, so we can eyeball how well the ranking holds up.

## Tuning plan

Thresholds in this doc (0.70 source-level, 0.60 merged floor, ±30 days, N = 20) are **starting points for data collection**, not final values. After running against a real corpus:

- Log every fuzzy invocation: disc identifiers in, full candidate list out, user's eventual pick (if any).
- Review the log to find:
  - The score gap between user-picked and runner-up candidates → informs T_auto.
  - The score of correct picks vs. the floor → informs T_show.
  - Whether the +0.05 agreement bonus actually predicts correctness.
- Tighten thresholds and shrink N in a follow-up pass.

All thresholds and the candidate cap should be **config knobs** from day one, not hardcoded constants, so tuning doesn't require a rebuild.

## Open questions

- Where to persist the fuzzy-match log (same dir as the existing miss log from `dbintegration.md` §4?).
- Whether to expose the score in the UI directly or just the ordering.
- How to handle ties at the top of the list (currently: stable order by `redump_id`).
