//! Candidate / commit policy: turn the decoder's ranked candidates into a preedit and a
//! suggestion list.
//!
//! Default policy (host spec §4.2): preview the top-1 word as a preedit and offer the
//! alternates in a suggestion bar; the backend finalizes the preedit on the next action
//! (space / next swipe / tap-away) and offers one-tap correction to an alternate. The top
//! score is a fused log-score (higher is better); a deployment can raise `min_confidence`
//! after tuning so weak/OOV results fall back to letter entry instead of forcing a wrong
//! word. A closed-vocabulary miss simply yields no candidates.

use shepherd_swipe_core::Candidate;

/// Tuning for how candidates become a preedit + suggestions.
#[derive(Debug, Clone, Copy)]
pub struct CandidatePolicy {
    /// Maximum number of suggestions to surface in the bar.
    pub max_suggestions: usize,
    /// Minimum top-candidate log-score to auto-preview it as a preedit. Defaults to
    /// "accept any candidate" (`NEG_INFINITY`); raise it after tuning to make low-confidence
    /// swipes fall back to letter entry rather than committing a guess.
    pub min_confidence: f32,
}

impl Default for CandidatePolicy {
    fn default() -> Self {
        Self {
            max_suggestions: 4,
            min_confidence: f32::NEG_INFINITY,
        }
    }
}

/// The policy's decision for one decode.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// The word to preview as a preedit, if the top candidate cleared `min_confidence`.
    pub preedit: Option<String>,
    /// Ordered alternates for the suggestion bar (includes the preedit word first).
    pub suggestions: Vec<String>,
    /// The top candidate's fused log-score, for a legible confidence display. `None` when
    /// there were no candidates.
    pub top_score: Option<f32>,
    /// Whether the top candidate cleared `min_confidence`.
    pub confident: bool,
}

impl Decision {
    /// A decision with nothing to show (no candidates / below threshold): the backend should
    /// fall back to letter entry.
    fn empty() -> Self {
        Self {
            preedit: None,
            suggestions: Vec::new(),
            top_score: None,
            confident: false,
        }
    }
}

impl CandidatePolicy {
    /// Apply the policy to a ranked candidate list (best first).
    pub fn decide(&self, candidates: &[Candidate]) -> Decision {
        let Some(top) = candidates.first() else {
            return Decision::empty();
        };
        let suggestions: Vec<String> = candidates
            .iter()
            .take(self.max_suggestions)
            .map(|c| c.word.clone())
            .collect();
        let confident = top.score >= self.min_confidence;
        Decision {
            preedit: confident.then(|| top.word.clone()),
            suggestions,
            top_score: Some(top.score),
            confident,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(word: &str, score: f32) -> Candidate {
        Candidate {
            word: word.to_string(),
            score,
        }
    }

    #[test]
    fn no_candidates_means_fall_back_to_letters() {
        let d = CandidatePolicy::default().decide(&[]);
        assert_eq!(d, Decision::empty());
        assert!(d.preedit.is_none());
        assert!(d.suggestions.is_empty());
    }

    #[test]
    fn default_policy_previews_top_one_and_lists_alternates() {
        let cands = [
            cand("hello", -9.5),
            cand("help", -14.2),
            cand("hell", -15.5),
            cand("held", -15.6),
            cand("hero", -19.0),
        ];
        let d = CandidatePolicy::default().decide(&cands);
        assert_eq!(d.preedit.as_deref(), Some("hello"));
        assert_eq!(d.suggestions, ["hello", "help", "hell", "held"]); // capped at 4
        assert_eq!(d.top_score, Some(-9.5));
        assert!(d.confident);
    }

    #[test]
    fn raised_threshold_suppresses_preedit_but_still_offers_suggestions() {
        let policy = CandidatePolicy {
            min_confidence: -10.0,
            ..CandidatePolicy::default()
        };
        let weak = policy.decide(&[cand("xÿz", -25.0), cand("xyz", -26.0)]);
        assert!(!weak.confident);
        assert!(weak.preedit.is_none());
        assert_eq!(weak.suggestions, ["xÿz", "xyz"]);

        let strong = policy.decide(&[cand("hello", -9.5)]);
        assert!(strong.confident);
        assert_eq!(strong.preedit.as_deref(), Some("hello"));
    }
}
