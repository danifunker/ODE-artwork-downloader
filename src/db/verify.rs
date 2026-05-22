//! Candidate verification by disc-content inspection.
//!
//! When fuzzy matching produces several high-scoring candidates, the disc
//! itself is the tiebreaker: walk its file tree, read small identity-style
//! files, and check which candidate (if any) the disc's content actually
//! supports. Three outcomes per the design:
//!
//! - **Corroborate** — at least one candidate's distinctive title tokens (or
//!   creation date) match the evidence. Keep those candidates, drop the rest.
//! - **Contradict** — the disc *does* have readable identity text, but no
//!   candidate's tokens appear. Drop everything; we earned the rule-out by
//!   finding usable identity that disagreed.
//! - **Abstain** — the disc is mostly opaque (binaries, cab files, no readme).
//!   We can't tell, so we leave the candidate list as-is.
//!
//! Bounded by a byte budget so we don't read the whole disc.

use std::collections::HashSet;

use crate::db::fuzzy::FuzzyCandidate;
use crate::disc::{read_content, DiscContent, DiscInfo};

/// What we learned from looking at the disc. Thin re-export of `DiscContent`'s
/// fields so the verifier presents a stable surface.
#[derive(Debug, Default)]
pub struct DiscEvidence {
    pub tokens: HashSet<String>,
    pub creation_date: Option<String>,
    pub files_seen: usize,
    pub bytes_read: u64,
    pub usable_identity: bool,
}

impl From<DiscContent> for DiscEvidence {
    fn from(c: DiscContent) -> Self {
        let usable_identity = c.usable_identity();
        Self {
            tokens: c.tokens,
            creation_date: c.creation_date,
            files_seen: c.files_seen,
            bytes_read: c.bytes_read,
            usable_identity,
        }
    }
}

/// Gather evidence from a disc. Walks the filesystem (bounded), reads
/// identity-style text files, and returns a flat token set + creation date.
pub fn gather_evidence(info: &DiscInfo) -> DiscEvidence {
    read_content(info).into()
}

/// What the verifier decided per candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Some piece of evidence supports this candidate.
    Corroborated,
    /// Evidence is rich but doesn't mention this candidate's distinctive
    /// tokens. Drop it.
    Contradicted,
    /// Evidence is too sparse to judge; keep the candidate.
    Abstain,
}

/// Score one candidate against gathered evidence. Pure function for testing.
pub fn classify(candidate: &FuzzyCandidate, ev: &DiscEvidence) -> Verdict {
    // Creation date is decisive corroboration when the redump row also has
    // one and it matches.
    if let Some(disc_d) = ev.creation_date.as_ref() {
        if let Some(cand_d) = candidate.pvd_creation_date.as_deref() {
            if cand_d == disc_d.as_str() {
                return Verdict::Corroborated;
            }
        }
    }

    let title_tokens: Vec<String> = title_distinctive_tokens(&candidate.title);
    if title_tokens.is_empty() {
        // Nothing distinctive about the title; can't meaningfully verify.
        return Verdict::Abstain;
    }

    // Single-token titles are corroborated by a single hit; multi-token
    // titles need at least two of their distinctive tokens to appear so a
    // common shared word doesn't carry the day on its own.
    let need = if title_tokens.len() == 1 { 1 } else { 2 };
    let hits = title_tokens.iter().filter(|t| ev.tokens.contains(*t)).count();
    if hits >= need {
        return Verdict::Corroborated;
    }

    if ev.usable_identity {
        Verdict::Contradicted
    } else {
        Verdict::Abstain
    }
}

/// Apply the three-outcome rule to a candidate list. Returns the pruned list,
/// preserving the input ordering.
pub fn verify(
    candidates: Vec<FuzzyCandidate>,
    ev: &DiscEvidence,
) -> Vec<FuzzyCandidate> {
    if candidates.is_empty() {
        return candidates;
    }

    let verdicts: Vec<Verdict> = candidates.iter().map(|c| classify(c, ev)).collect();

    let any_corroborated = verdicts.iter().any(|v| *v == Verdict::Corroborated);

    if any_corroborated {
        // Keep only corroborated candidates (drop abstain + contradicted).
        candidates
            .into_iter()
            .zip(verdicts.iter())
            .filter(|(_, v)| **v == Verdict::Corroborated)
            .map(|(c, _)| c)
            .collect()
    } else if ev.usable_identity {
        // Rich evidence and nothing matched — drop everything.
        Vec::new()
    } else {
        // Sparse evidence — abstain, keep the input list as-is.
        candidates
    }
}

/// Extract distinctive title tokens for the verifier: alphanumeric, ≥3 chars,
/// not stopwords. Same rules as the evidence side so they're comparable.
fn title_distinctive_tokens(title: &str) -> Vec<String> {
    crate::disc::content::distinctive_tokens(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fc(id: i64, title: &str) -> FuzzyCandidate {
        FuzzyCandidate {
            redump_id: id,
            system: "pc".into(),
            title: title.into(),
            redump_url: String::new(),
            score: 0.95,
            sources: vec![crate::db::ScoreSource::Title],
            size_ratio: None,
            inferred_version: None,
            pvd_creation_date: None,
            match_reason: "test".into(),
            winworld: None,
        }
    }

    fn ev_with(tokens: &[&str]) -> DiscEvidence {
        let mut ev = DiscEvidence::default();
        for t in tokens {
            ev.tokens.insert((*t).into());
        }
        ev.usable_identity = ev.tokens.len() >= 5;
        ev
    }

    #[test]
    fn corroborates_when_two_title_tokens_present() {
        let cand = fc(1, "Mission Critical");
        let ev = ev_with(&["mission", "critical", "legend", "install", "data", "setup"]);
        assert_eq!(classify(&cand, &ev), Verdict::Corroborated);
    }

    #[test]
    fn contradicts_when_evidence_rich_but_no_match() {
        // Disc clearly has Visual Studio identity tokens; candidate is unrelated.
        let cand = fc(2, "Space Ace CD-ROM");
        let ev = ev_with(&[
            "visual", "studio", "msdev", "vc98", "microsoft", "developer",
        ]);
        assert_eq!(classify(&cand, &ev), Verdict::Contradicted);
    }

    #[test]
    fn abstains_on_sparse_evidence() {
        let cand = fc(3, "Some Random Game");
        let mut ev = DiscEvidence::default();
        ev.tokens.insert("abc".into());
        ev.usable_identity = false;
        assert_eq!(classify(&cand, &ev), Verdict::Abstain);
    }

    #[test]
    fn single_token_title_needs_only_one_hit() {
        let cand = fc(4, "Quake");
        let ev = ev_with(&["quake", "txt", "exe", "pak0", "models", "sounds"]);
        assert_eq!(classify(&cand, &ev), Verdict::Corroborated);
    }

    #[test]
    fn one_shared_common_word_in_multi_token_title_does_not_corroborate() {
        // "Software" appears, but "Tycoon" doesn't — only one of two
        // distinctive tokens, not enough.
        let cand = fc(5, "Software Tycoon");
        let ev = ev_with(&["software", "macos9", "apple", "system", "extensions", "control"]);
        assert_eq!(classify(&cand, &ev), Verdict::Contradicted);
    }

    #[test]
    fn filename_tokens_corroborate_when_disc_data_is_opaque() {
        // Simulate a data-only disc: volume label adds "mission" but no
        // "critical" anywhere on disc. The filename ("Mission Critical
        // (Disc 2 of 3)") contributes "critical", reaching 2-of-2.
        let cand = fc(1, "Mission Critical (Disc 2)");
        let ev = ev_with(&["mission", "critical", "legend", "mc001", "dos4gw"]);
        assert_eq!(classify(&cand, &ev), Verdict::Corroborated);
    }

    #[test]
    fn verify_drops_all_competitors_when_one_corroborates() {
        let cands = vec![
            fc(10, "Mission Critical"),
            fc(11, "Combat Mission"),
            fc(12, "Mission to McDonaldland"),
        ];
        let ev = ev_with(&[
            "mission", "critical", "legend", "install", "data", "setup",
        ]);
        let kept = verify(cands, &ev);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].redump_id, 10);
    }

    #[test]
    fn verify_drops_all_on_contradiction() {
        let cands = vec![fc(20, "FMV Taikenban"), fc(21, "Trivial Pursuit")];
        let ev = ev_with(&["visual", "studio", "msdev", "vc98", "microsoft", "library"]);
        let kept = verify(cands, &ev);
        assert!(kept.is_empty());
    }

    #[test]
    fn verify_keeps_all_on_abstention() {
        let cands = vec![fc(30, "Anything"), fc(31, "Else")];
        let mut ev = DiscEvidence::default();
        ev.usable_identity = false;
        let kept = verify(cands, &ev);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn distinctive_tokens_lowercases_and_skips_short() {
        let v = crate::disc::content::distinctive_tokens("MSDEV98 a Visual Studio 6.0");
        assert!(v.contains(&"msdev98".to_string()));
        assert!(v.contains(&"visual".to_string()));
        assert!(v.contains(&"studio".to_string()));
        assert!(!v.contains(&"a".to_string()));
        assert!(!v.contains(&"6".to_string()));
    }

    #[test]
    fn stopwords_excluded() {
        let v = crate::disc::content::distinctive_tokens("the and for windows new version");
        assert!(v.is_empty());
    }
}
