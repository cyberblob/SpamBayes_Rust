//! Bayesian spam classifier using chi-squared combining.
//!
//! This module implements the core `SpamBayes` classifier, which computes
//! per-token spam probabilities using Robinson's method and combines them
//! via chi-squared statistics.

use std::collections::{HashMap, HashSet};

use crate::{chi2, Classification, ClassifierConfig, ClassifierError, ScoreResult, WordInfo};

/// Core Bayesian spam classifier using chi-squared combining.
pub struct Classifier {
    /// Per-token spam/ham occurrence counts.
    pub(crate) word_info: HashMap<Vec<u8>, WordInfo>,
    /// Total number of spam messages trained.
    pub(crate) nspam: u64,
    /// Total number of ham messages trained.
    pub(crate) nham: u64,
    /// Cache of probability results keyed by (`spam_count`, `ham_count`).
    pub(crate) prob_cache: HashMap<(u32, u32), f64>,
    /// Configuration parameters for probability calculation.
    pub(crate) config: ClassifierConfig,
}

impl Classifier {
    /// Create a new, empty classifier with the given configuration.
    #[must_use]
    pub fn new(config: ClassifierConfig) -> Self {
        Self {
            word_info: HashMap::new(),
            nspam: 0,
            nham: 0,
            prob_cache: HashMap::new(),
            config,
        }
    }

    /// Create a classifier pre-loaded with state and token data.
    ///
    /// Used when restoring from persistent storage.
    #[must_use]
    pub fn from_state(
        config: ClassifierConfig,
        nspam: u64,
        nham: u64,
        word_info: HashMap<Vec<u8>, WordInfo>,
    ) -> Self {
        Self {
            word_info,
            nspam,
            nham,
            prob_cache: HashMap::new(),
            config,
        }
    }

    /// Create a new classifier with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(ClassifierConfig::default())
    }

    /// Compute Robinson-adjusted spam probability for a single token.
    ///
    /// The formula is:
    ///   raw = (`spam_count` / nspam) / ((`spam_count` / nspam) + (`ham_count` / nham))
    ///   f(w) = (S * x + n * raw) / (S + n)
    ///
    /// Where:
    ///   S = `unknown_word_strength` (default 0.45)
    ///   x = `unknown_word_prob` (default 0.5)
    ///   n = `spam_count` + `ham_count`
    ///
    /// Edge cases:
    /// - When nspam or nham is 0, we use max(1) to avoid division by zero.
    /// - When a token has never been seen (`spam_count=0`, `ham_count=0`), the
    ///   formula yields `unknown_word_prob` (since n=0 and raw is irrelevant).
    #[must_use]
    pub fn probability(&self, record: &WordInfo) -> f64 {
        // Check the cache first
        let cache_key = (record.spam_count, record.ham_count);
        if let Some(&cached) = self.prob_cache.get(&cache_key) {
            return cached;
        }

        self.compute_probability(record)
    }

    /// Internal probability computation without cache lookup.
    fn compute_probability(&self, record: &WordInfo) -> f64 {
        // Token never seen in training → assign unknown_word_prob (Requirement 4.10)
        if record.spam_count == 0 && record.ham_count == 0 {
            return self.config.unknown_word_prob;
        }

        // Use max(1) to prevent division by zero when no messages trained
        let nham = (self.nham.max(1)) as f64;
        let nspam = (self.nspam.max(1)) as f64;

        let ham_ratio = f64::from(record.ham_count) / nham;
        let spam_ratio = f64::from(record.spam_count) / nspam;

        let raw_prob = spam_ratio / (ham_ratio + spam_ratio);

        let s = self.config.unknown_word_strength;
        let s_times_x = s * self.config.unknown_word_prob;
        let n = f64::from(record.spam_count + record.ham_count);

        (s_times_x + n * raw_prob) / (s + n)
    }

    /// Compute probability and store in cache. Returns the probability.
    pub fn probability_cached(&mut self, record: &WordInfo) -> f64 {
        let cache_key = (record.spam_count, record.ham_count);
        if let Some(&cached) = self.prob_cache.get(&cache_key) {
            return cached;
        }

        let prob = self.compute_probability(record);
        self.prob_cache.insert(cache_key, prob);
        prob
    }

    /// Clear the probability cache (should be called after training changes).
    pub fn clear_cache(&mut self) {
        self.prob_cache.clear();
    }

    /// Returns the number of spam messages trained.
    #[must_use]
    pub fn nspam(&self) -> u64 {
        self.nspam
    }

    /// Returns the number of ham messages trained.
    #[must_use]
    pub fn nham(&self) -> u64 {
        self.nham
    }

    /// Returns a reference to the word info map.
    #[must_use]
    pub fn word_info(&self) -> &HashMap<Vec<u8>, WordInfo> {
        &self.word_info
    }

    /// Returns a reference to the classifier configuration.
    #[must_use]
    pub fn config(&self) -> &ClassifierConfig {
        &self.config
    }

    /// Select the most significant tokens for scoring.
    ///
    /// This method deduplicates the input tokens, computes each token's spam
    /// probability, filters out tokens whose distance from 0.5 is less than
    /// `minimum_prob_strength`, sorts by distance from 0.5 in descending order,
    /// and truncates to at most `max_discriminators` tokens.
    ///
    /// Returns a `Vec` of `(probability, token)` pairs.
    pub fn get_clues(&self, tokens: impl Iterator<Item = Vec<u8>>) -> Vec<(f64, Vec<u8>)> {
        // Deduplicate tokens
        let unique_tokens: HashSet<Vec<u8>> = tokens.collect();

        let min_dist = self.config.minimum_prob_strength;
        let max_disc = self.config.max_discriminators;

        // Compute probability for each unique token, filter by minimum distance from 0.5
        let mut clues: Vec<(f64, Vec<u8>)> = unique_tokens
            .into_iter()
            .filter_map(|token| {
                let record = self
                    .word_info
                    .get(&token)
                    .copied()
                    .unwrap_or(WordInfo { spam_count: 0, ham_count: 0 });
                let prob = self.probability(&record);
                let distance = (prob - 0.5).abs();
                if distance >= min_dist {
                    Some((prob, token))
                } else {
                    None
                }
            })
            .collect();

        // Sort by distance from 0.5 in descending order (most discriminative first)
        clues.sort_by(|a, b| {
            let dist_a = (a.0 - 0.5).abs();
            let dist_b = (b.0 - 0.5).abs();
            dist_b.partial_cmp(&dist_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Truncate to max_discriminators
        clues.truncate(max_disc);

        clues
    }

    /// Compute the overall spam probability for a set of tokens.
    ///
    /// Selects the most discriminative tokens via `get_clues`, then combines
    /// their probabilities using chi-squared statistics. Returns a value in
    /// `0.0..=1.0` where higher means more likely spam.
    ///
    /// Returns `0.5` (neutral) if no significant clues are found.
    pub fn spam_prob(&self, tokens: impl Iterator<Item = Vec<u8>>) -> f64 {
        let clues = self.get_clues(tokens);

        if clues.is_empty() {
            return 0.5;
        }

        let probs: Vec<f64> = clues.iter().map(|(p, _)| *p).collect();
        let (_, _, score) = chi2::chi2_combine(&probs);
        score
    }

    /// Compute spam probability with full evidence (clues list).
    ///
    /// Like `spam_prob`, but also returns the contributing clues and a
    /// placeholder classification. The classifier does not apply threshold
    /// logic — that is the filter engine's responsibility — so classification
    /// is always `Unsure`.
    pub fn spam_prob_with_evidence(&self, tokens: impl Iterator<Item = Vec<u8>>) -> ScoreResult {
        let clues = self.get_clues(tokens);

        if clues.is_empty() {
            return ScoreResult {
                probability: 0.5,
                classification: Classification::Unsure,
                clues: Some(vec![]),
            };
        }

        let probs: Vec<f64> = clues.iter().map(|(p, _)| *p).collect();
        let (_, _, score) = chi2::chi2_combine(&probs);

        let clue_list: Vec<(String, f64)> = clues
            .into_iter()
            .map(|(prob, token)| {
                let name = String::from_utf8_lossy(&token).into_owned();
                (name, prob)
            })
            .collect();

        ScoreResult {
            probability: score,
            classification: Classification::Unsure,
            clues: Some(clue_list),
        }
    }

    /// Train the classifier on a message's tokens as spam or ham.
    ///
    /// Tokens are deduplicated so each token is counted at most once per
    /// message, regardless of how many times it appears. After training,
    /// the probability cache is cleared.
    pub fn learn(&mut self, tokens: impl Iterator<Item = Vec<u8>>, is_spam: bool) {
        // Deduplicate tokens (Requirement 4.1)
        let unique_tokens: HashSet<Vec<u8>> = tokens.collect();

        // Increment global message count (Requirements 4.5, 4.6)
        if is_spam {
            self.nspam += 1;
        } else {
            self.nham += 1;
        }

        // Increment per-token counts for each unique token
        for token in unique_tokens {
            let record = self.word_info.entry(token).or_default();
            if is_spam {
                record.spam_count += 1;
            } else {
                record.ham_count += 1;
            }
        }

        // Clear cache since training data changed
        self.clear_cache();
    }

    /// Untrain a previously learned message.
    ///
    /// Validates that untraining won't cause global counts to go negative,
    /// then decrements global and per-token counts. Removes token records
    /// where both spam and ham counts reach zero. Clears the probability
    /// cache after untraining.
    ///
    /// # Errors
    ///
    /// Returns `ClassifierError::InvalidOperation` if untraining would
    /// cause the global spam or ham count to go below zero.
    pub fn unlearn(
        &mut self,
        tokens: impl Iterator<Item = Vec<u8>>,
        is_spam: bool,
    ) -> Result<(), ClassifierError> {
        // Validate global count won't go negative (Requirement 4.8)
        if is_spam {
            if self.nspam == 0 {
                return Err(ClassifierError::InvalidOperation(
                    "spam message count would go below zero".to_string(),
                ));
            }
        } else if self.nham == 0 {
            return Err(ClassifierError::InvalidOperation(
                "ham message count would go below zero".to_string(),
            ));
        }

        // Deduplicate tokens (Requirement 4.1)
        let unique_tokens: HashSet<Vec<u8>> = tokens.collect();

        // Decrement global count (Requirement 4.7)
        if is_spam {
            self.nspam -= 1;
        } else {
            self.nham -= 1;
        }

        // Decrement per-token counts and remove zero-count records
        for token in unique_tokens {
            if let Some(record) = self.word_info.get_mut(&token) {
                if is_spam {
                    record.spam_count = record.spam_count.saturating_sub(1);
                } else {
                    record.ham_count = record.ham_count.saturating_sub(1);
                }

                // Remove record if both counts are zero (Requirement 4.7)
                if record.spam_count == 0 && record.ham_count == 0 {
                    self.word_info.remove(&token);
                }
            }
        }

        // Clear cache since training data changed
        self.clear_cache();

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test assertions comparing exact threshold values
mod tests {
    use super::*;

    /// Helper to create a classifier with specific training counts.
    fn classifier_with_counts(nspam: u64, nham: u64) -> Classifier {
        let mut c = Classifier::with_defaults();
        c.nspam = nspam;
        c.nham = nham;
        c
    }

    /// Helper to create a classifier with custom config.
    fn classifier_with_config(nspam: u64, nham: u64, s: f64, x: f64) -> Classifier {
        let config = ClassifierConfig {
            unknown_word_strength: s,
            unknown_word_prob: x,
            ..ClassifierConfig::default()
        };
        let mut c = Classifier::new(config);
        c.nspam = nspam;
        c.nham = nham;
        c
    }

    // ─── Basic probability tests ─────────────────────────────────────────

    #[test]
    fn test_unseen_token_returns_unknown_word_prob() {
        // A token never seen in training should get unknown_word_prob (0.5)
        // When spam_count=0 and ham_count=0, n=0, so:
        // f(w) = (S * x + 0 * raw) / (S + 0) = x = 0.5
        let c = classifier_with_counts(100, 100);
        let record = WordInfo {
            spam_count: 0,
            ham_count: 0,
        };
        let prob = c.probability(&record);
        assert!(
            (prob - 0.5).abs() < 1e-10,
            "unseen token should be 0.5, got {prob}"
        );
    }

    #[test]
    fn test_pure_spam_token() {
        // Token seen only in spam: spam_count=10, ham_count=0
        // raw = (10/100) / ((10/100) + (0/100)) = 0.1 / 0.1 = 1.0
        // n = 10
        // f(w) = (0.45*0.5 + 10*1.0) / (0.45 + 10) = (0.225 + 10) / 10.45
        let c = classifier_with_counts(100, 100);
        let record = WordInfo {
            spam_count: 10,
            ham_count: 0,
        };
        let prob = c.probability(&record);
        let expected = (0.45 * 0.5 + 10.0 * 1.0) / (0.45 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "pure spam token: expected {expected}, got {prob}"
        );
    }

    #[test]
    fn test_pure_ham_token() {
        // Token seen only in ham: spam_count=0, ham_count=10
        // raw = (0/100) / ((0/100) + (10/100)) = 0.0 / 0.1 = 0.0
        // n = 10
        // f(w) = (0.45*0.5 + 10*0.0) / (0.45 + 10) = 0.225 / 10.45
        let c = classifier_with_counts(100, 100);
        let record = WordInfo {
            spam_count: 0,
            ham_count: 10,
        };
        let prob = c.probability(&record);
        let expected = (0.45 * 0.5 + 10.0 * 0.0) / (0.45 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "pure ham token: expected {expected}, got {prob}"
        );
    }

    #[test]
    fn test_equal_spam_ham_counts_equal_training() {
        // Token seen equally in spam and ham with equal training counts
        // spam_count=5, ham_count=5, nspam=100, nham=100
        // raw = (5/100) / ((5/100) + (5/100)) = 0.05 / 0.1 = 0.5
        // n = 10
        // f(w) = (0.45*0.5 + 10*0.5) / (0.45 + 10) = (0.225 + 5.0) / 10.45
        let c = classifier_with_counts(100, 100);
        let record = WordInfo {
            spam_count: 5,
            ham_count: 5,
        };
        let prob = c.probability(&record);
        let expected = (0.45 * 0.5 + 10.0 * 0.5) / (0.45 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "equal counts: expected {expected}, got {prob}"
        );
        // Should be close to 0.5
        assert!((prob - 0.5).abs() < 0.01);
    }

    // ─── Edge cases: nspam=0 or nham=0 ──────────────────────────────────

    #[test]
    fn test_nspam_zero_uses_max_one() {
        // When nspam=0, we use max(1) = 1 to avoid division issues
        let c = classifier_with_counts(0, 100);
        let record = WordInfo {
            spam_count: 5,
            ham_count: 5,
        };
        let prob = c.probability(&record);
        // nspam becomes 1: spam_ratio = 5/1 = 5.0
        // ham_ratio = 5/100 = 0.05
        // raw = 5.0 / (0.05 + 5.0) = 5.0 / 5.05
        // n = 10
        // f(w) = (0.225 + 10 * (5.0/5.05)) / 10.45
        let raw = 5.0 / 5.05;
        let expected = (0.45 * 0.5 + 10.0 * raw) / (0.45 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "nspam=0: expected {expected}, got {prob}"
        );
    }

    #[test]
    fn test_nham_zero_uses_max_one() {
        // When nham=0, we use max(1) = 1 to avoid division issues
        let c = classifier_with_counts(100, 0);
        let record = WordInfo {
            spam_count: 5,
            ham_count: 5,
        };
        let prob = c.probability(&record);
        // nham becomes 1: ham_ratio = 5/1 = 5.0
        // spam_ratio = 5/100 = 0.05
        // raw = 0.05 / (5.0 + 0.05) = 0.05 / 5.05
        // n = 10
        let raw = 0.05 / 5.05;
        let expected = (0.45 * 0.5 + 10.0 * raw) / (0.45 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "nham=0: expected {expected}, got {prob}"
        );
    }

    #[test]
    fn test_both_nspam_nham_zero() {
        // Both training counts zero: uses max(1) for both
        let c = classifier_with_counts(0, 0);
        let record = WordInfo {
            spam_count: 3,
            ham_count: 7,
        };
        let prob = c.probability(&record);
        // nspam=1, nham=1
        // spam_ratio = 3/1 = 3.0, ham_ratio = 7/1 = 7.0
        // raw = 3.0 / (7.0 + 3.0) = 0.3
        // n = 10
        let raw = 3.0 / 10.0;
        let expected = (0.45 * 0.5 + 10.0 * raw) / (0.45 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "both zero: expected {expected}, got {prob}"
        );
    }

    // ─── Robinson S and x parameter influence ────────────────────────────

    #[test]
    fn test_high_unknown_word_strength_pulls_toward_x() {
        // With high S, the result is pulled strongly toward x
        let c = classifier_with_config(100, 100, 10.0, 0.5);
        let record = WordInfo {
            spam_count: 10,
            ham_count: 0,
        };
        let prob = c.probability(&record);
        // raw = 1.0, n = 10
        // f(w) = (10*0.5 + 10*1.0) / (10 + 10) = 15/20 = 0.75
        let expected = (10.0 * 0.5 + 10.0 * 1.0) / (10.0 + 10.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "high S: expected {expected}, got {prob}"
        );
    }

    #[test]
    fn test_zero_unknown_word_strength_uses_pure_raw() {
        // With S=0, result is purely the raw probability (no prior influence)
        let c = classifier_with_config(100, 100, 0.0, 0.5);
        let record = WordInfo {
            spam_count: 8,
            ham_count: 2,
        };
        let prob = c.probability(&record);
        // raw = (8/100) / ((8/100) + (2/100)) = 0.08 / 0.10 = 0.8
        // f(w) = (0*0.5 + 10*0.8) / (0 + 10) = 8/10 = 0.8
        let expected = 0.8;
        assert!(
            (prob - expected).abs() < 1e-10,
            "S=0: expected {expected}, got {prob}"
        );
    }

    #[test]
    fn test_custom_unknown_word_prob() {
        // With x=0.2, unseen tokens should return 0.2
        let c = classifier_with_config(100, 100, 0.45, 0.2);
        let record = WordInfo {
            spam_count: 0,
            ham_count: 0,
        };
        let prob = c.probability(&record);
        // n=0: f(w) = (S*x) / S = x = 0.2
        assert!(
            (prob - 0.2).abs() < 1e-10,
            "custom x: expected 0.2, got {prob}"
        );
    }

    // ─── Asymmetric training counts ──────────────────────────────────────

    #[test]
    fn test_unequal_training_counts() {
        // nspam=200, nham=50 — training imbalance affects raw probability
        let c = classifier_with_counts(200, 50);
        let record = WordInfo {
            spam_count: 10,
            ham_count: 10,
        };
        let prob = c.probability(&record);
        // spam_ratio = 10/200 = 0.05
        // ham_ratio = 10/50 = 0.2
        // raw = 0.05 / (0.2 + 0.05) = 0.05 / 0.25 = 0.2
        // n = 20
        // f(w) = (0.225 + 20*0.2) / (0.45 + 20) = (0.225 + 4.0) / 20.45
        let raw = 0.05 / 0.25;
        let expected = (0.45 * 0.5 + 20.0 * raw) / (0.45 + 20.0);
        assert!(
            (prob - expected).abs() < 1e-10,
            "unequal training: expected {expected}, got {prob}"
        );
        // With more spam training, equal token counts should appear ham-like
        assert!(prob < 0.5);
    }

    // ─── Probability caching ─────────────────────────────────────────────

    #[test]
    fn test_probability_cached_returns_same_value() {
        let mut c = classifier_with_counts(100, 100);
        let record = WordInfo {
            spam_count: 7,
            ham_count: 3,
        };
        let prob1 = c.probability_cached(&record);
        let prob2 = c.probability_cached(&record);
        assert_eq!(prob1, prob2);
        // Verify cache was populated
        assert!(c.prob_cache.contains_key(&(7, 3)));
    }

    #[test]
    fn test_clear_cache() {
        let mut c = classifier_with_counts(100, 100);
        let record = WordInfo {
            spam_count: 7,
            ham_count: 3,
        };
        c.probability_cached(&record);
        assert!(!c.prob_cache.is_empty());
        c.clear_cache();
        assert!(c.prob_cache.is_empty());
    }

    // ─── Constructor tests ───────────────────────────────────────────────

    #[test]
    fn test_new_classifier_is_empty() {
        let c = Classifier::with_defaults();
        assert_eq!(c.nspam(), 0);
        assert_eq!(c.nham(), 0);
        assert!(c.word_info().is_empty());
        assert!(c.prob_cache.is_empty());
    }

    #[test]
    fn test_new_classifier_uses_default_config() {
        let c = Classifier::with_defaults();
        assert!((c.config().unknown_word_strength - 0.45).abs() < 1e-10);
        assert!((c.config().unknown_word_prob - 0.5).abs() < 1e-10);
        assert_eq!(c.config().max_discriminators, 150);
        assert!((c.config().minimum_prob_strength - 0.1).abs() < 1e-10);
    }

    // ─── Probability range validation ────────────────────────────────────

    #[test]
    fn test_probability_always_in_valid_range() {
        let c = classifier_with_counts(50, 150);
        let test_cases = vec![
            WordInfo { spam_count: 0, ham_count: 0 },
            WordInfo { spam_count: 1, ham_count: 0 },
            WordInfo { spam_count: 0, ham_count: 1 },
            WordInfo { spam_count: 100, ham_count: 0 },
            WordInfo { spam_count: 0, ham_count: 100 },
            WordInfo { spam_count: 50, ham_count: 50 },
            WordInfo { spam_count: 1, ham_count: 1000 },
            WordInfo { spam_count: 1000, ham_count: 1 },
        ];

        for record in &test_cases {
            let prob = c.probability(record);
            assert!(
                (0.0..=1.0).contains(&prob),
                "probability {prob} out of range for {record:?}"
            );
            assert!(
                !prob.is_nan(),
                "probability is NaN for {record:?}"
            );
        }
    }

    // ─── Monotonicity: more spam evidence → higher probability ───────────

    #[test]
    fn test_more_spam_count_increases_probability() {
        let c = classifier_with_counts(100, 100);
        let r1 = WordInfo { spam_count: 1, ham_count: 5 };
        let r2 = WordInfo { spam_count: 5, ham_count: 5 };
        let r3 = WordInfo { spam_count: 10, ham_count: 5 };

        let p1 = c.probability(&r1);
        let p2 = c.probability(&r2);
        let p3 = c.probability(&r3);

        assert!(p1 < p2, "p1={p1} should be < p2={p2}");
        assert!(p2 < p3, "p2={p2} should be < p3={p3}");
    }

    #[test]
    fn test_more_ham_count_decreases_probability() {
        let c = classifier_with_counts(100, 100);
        let r1 = WordInfo { spam_count: 5, ham_count: 1 };
        let r2 = WordInfo { spam_count: 5, ham_count: 5 };
        let r3 = WordInfo { spam_count: 5, ham_count: 10 };

        let p1 = c.probability(&r1);
        let p2 = c.probability(&r2);
        let p3 = c.probability(&r3);

        assert!(p1 > p2, "p1={p1} should be > p2={p2}");
        assert!(p2 > p3, "p2={p2} should be > p3={p3}");
    }

    // ─── get_clues tests ─────────────────────────────────────────────────

    /// Helper to build a classifier with known `word_info` entries.
    fn classifier_with_words(
        nspam: u64,
        nham: u64,
        words: Vec<(&[u8], u32, u32)>,
    ) -> Classifier {
        let mut c = classifier_with_counts(nspam, nham);
        for (token, spam, ham) in words {
            c.word_info.insert(
                token.to_vec(),
                WordInfo { spam_count: spam, ham_count: ham },
            );
        }
        c
    }

    #[test]
    fn test_get_clues_empty_tokens() {
        let c = classifier_with_counts(100, 100);
        let clues = c.get_clues(std::iter::empty());
        assert!(clues.is_empty());
    }

    #[test]
    fn test_get_clues_unknown_tokens_filtered_out() {
        // Unknown tokens get probability 0.5, distance 0.0 < 0.1, so filtered
        let c = classifier_with_counts(100, 100);
        let tokens = vec![b"unknown1".to_vec(), b"unknown2".to_vec()];
        let clues = c.get_clues(tokens.into_iter());
        assert!(clues.is_empty());
    }

    #[test]
    fn test_get_clues_deduplicates_tokens() {
        // Same token repeated should only appear once in result
        let c = classifier_with_words(100, 100, vec![
            (b"spam_word", 20, 0), // strongly spammy
        ]);
        let tokens = vec![
            b"spam_word".to_vec(),
            b"spam_word".to_vec(),
            b"spam_word".to_vec(),
        ];
        let clues = c.get_clues(tokens.into_iter());
        assert_eq!(clues.len(), 1);
        assert_eq!(clues[0].1, b"spam_word".to_vec());
    }

    #[test]
    fn test_get_clues_filters_by_min_distance() {
        // Token with probability very close to 0.5 should be filtered out
        // Token with high distance should be kept
        let c = classifier_with_words(100, 100, vec![
            (b"strong_spam", 20, 0),  // probability far from 0.5
            (b"weak_token", 5, 5),    // probability ~0.5, distance < 0.1
        ]);
        let tokens = vec![b"strong_spam".to_vec(), b"weak_token".to_vec()];
        let clues = c.get_clues(tokens.into_iter());

        // Only the strong token should survive filtering
        assert_eq!(clues.len(), 1);
        assert_eq!(clues[0].1, b"strong_spam".to_vec());
    }

    #[test]
    fn test_get_clues_sorted_by_distance_descending() {
        let c = classifier_with_words(100, 100, vec![
            (b"medium_spam", 10, 2),  // moderately spammy
            (b"strong_spam", 20, 0),  // very spammy (closer to 1.0)
            (b"strong_ham", 0, 20),   // very hammy (closer to 0.0)
        ]);
        let tokens = vec![
            b"medium_spam".to_vec(),
            b"strong_spam".to_vec(),
            b"strong_ham".to_vec(),
        ];
        let clues = c.get_clues(tokens.into_iter());

        assert_eq!(clues.len(), 3);
        // Verify sorted by distance from 0.5 (descending)
        let distances: Vec<f64> = clues.iter().map(|(p, _)| (p - 0.5).abs()).collect();
        for i in 0..distances.len() - 1 {
            assert!(
                distances[i] >= distances[i + 1],
                "clues not sorted by distance: {distances:?}"
            );
        }
    }

    #[test]
    fn test_get_clues_truncates_to_max_discriminators() {
        // Create classifier with max_discriminators = 3 for testing
        let config = ClassifierConfig {
            max_discriminators: 3,
            ..ClassifierConfig::default()
        };
        let mut c = Classifier::new(config);
        c.nspam = 100;
        c.nham = 100;

        // Insert 5 strongly discriminative tokens
        for i in 0..5u32 {
            let token = format!("token_{i}").into_bytes();
            c.word_info.insert(
                token,
                WordInfo { spam_count: 15 + i, ham_count: 0 },
            );
        }

        let tokens: Vec<Vec<u8>> = (0..5).map(|i| format!("token_{i}").into_bytes()).collect();
        let clues = c.get_clues(tokens.into_iter());

        // Should be truncated to 3
        assert_eq!(clues.len(), 3);
    }

    #[test]
    fn test_get_clues_returns_probability_and_token() {
        let c = classifier_with_words(100, 100, vec![
            (b"spammy", 15, 0),
        ]);
        let tokens = vec![b"spammy".to_vec()];
        let clues = c.get_clues(tokens.into_iter());

        assert_eq!(clues.len(), 1);
        let (prob, token) = &clues[0];
        assert_eq!(token, &b"spammy".to_vec());
        // Verify probability matches what probability() returns
        let expected_prob = c.probability(&WordInfo { spam_count: 15, ham_count: 0 });
        assert!((prob - expected_prob).abs() < 1e-10);
    }

    #[test]
    fn test_get_clues_with_default_config_max_150() {
        // With default config, max_discriminators = 150
        let mut c = Classifier::with_defaults();
        c.nspam = 100;
        c.nham = 100;

        // Insert 200 distinct strongly spammy tokens
        for i in 0..200u32 {
            let token = format!("word_{i:03}").into_bytes();
            c.word_info.insert(
                token,
                WordInfo { spam_count: 10 + (i % 20), ham_count: 0 },
            );
        }

        let tokens: Vec<Vec<u8>> = (0..200).map(|i| format!("word_{i:03}").into_bytes()).collect();
        let clues = c.get_clues(tokens.into_iter());

        // Should be at most 150
        assert_eq!(clues.len(), 150);
    }

    // ─── spam_prob tests ─────────────────────────────────────────────────

    #[test]
    fn test_spam_prob_empty_tokens_returns_neutral() {
        let c = classifier_with_counts(100, 100);
        let score = c.spam_prob(std::iter::empty());
        assert!(
            (score - 0.5).abs() < 1e-10,
            "empty tokens should return 0.5, got {score}"
        );
    }

    #[test]
    fn test_spam_prob_unknown_tokens_returns_neutral() {
        // All unknown tokens get filtered out by get_clues (distance < 0.1)
        let c = classifier_with_counts(100, 100);
        let tokens = vec![b"never_seen".to_vec(), b"also_unknown".to_vec()];
        let score = c.spam_prob(tokens.into_iter());
        assert!(
            (score - 0.5).abs() < 1e-10,
            "unknown tokens should return 0.5, got {score}"
        );
    }

    #[test]
    fn test_spam_prob_spammy_tokens_high_score() {
        let c = classifier_with_words(100, 100, vec![
            (b"buy_now", 20, 0),
            (b"free_money", 18, 0),
            (b"viagra", 25, 0),
        ]);
        let tokens = vec![
            b"buy_now".to_vec(),
            b"free_money".to_vec(),
            b"viagra".to_vec(),
        ];
        let score = c.spam_prob(tokens.into_iter());
        assert!(score > 0.9, "spammy tokens should score high, got {score}");
    }

    #[test]
    fn test_spam_prob_hammy_tokens_low_score() {
        let c = classifier_with_words(100, 100, vec![
            (b"meeting", 0, 20),
            (b"agenda", 0, 18),
            (b"minutes", 0, 25),
        ]);
        let tokens = vec![
            b"meeting".to_vec(),
            b"agenda".to_vec(),
            b"minutes".to_vec(),
        ];
        let score = c.spam_prob(tokens.into_iter());
        assert!(score < 0.1, "hammy tokens should score low, got {score}");
    }

    #[test]
    fn test_spam_prob_in_valid_range() {
        let c = classifier_with_words(100, 100, vec![
            (b"word_a", 12, 3),
            (b"word_b", 2, 15),
            (b"word_c", 8, 8),
        ]);
        let tokens = vec![
            b"word_a".to_vec(),
            b"word_b".to_vec(),
            b"word_c".to_vec(),
        ];
        let score = c.spam_prob(tokens.into_iter());
        assert!(
            (0.0..=1.0).contains(&score),
            "score {score} out of [0, 1] range"
        );
    }

    // ─── spam_prob_with_evidence tests ───────────────────────────────────

    #[test]
    fn test_spam_prob_with_evidence_empty_tokens() {
        let c = classifier_with_counts(100, 100);
        let result = c.spam_prob_with_evidence(std::iter::empty());
        assert!((result.probability - 0.5).abs() < 1e-10);
        assert_eq!(result.classification, Classification::Unsure);
        assert_eq!(result.clues, Some(vec![]));
    }

    #[test]
    fn test_spam_prob_with_evidence_unknown_tokens() {
        let c = classifier_with_counts(100, 100);
        let tokens = vec![b"unknown".to_vec()];
        let result = c.spam_prob_with_evidence(tokens.into_iter());
        assert!((result.probability - 0.5).abs() < 1e-10);
        assert_eq!(result.classification, Classification::Unsure);
        assert_eq!(result.clues, Some(vec![]));
    }

    #[test]
    fn test_spam_prob_with_evidence_returns_clues() {
        let c = classifier_with_words(100, 100, vec![
            (b"spammy", 20, 0),
            (b"hammy", 0, 20),
        ]);
        let tokens = vec![b"spammy".to_vec(), b"hammy".to_vec()];
        let result = c.spam_prob_with_evidence(tokens.into_iter());

        assert_eq!(result.classification, Classification::Unsure);
        let clues = result.clues.unwrap();
        assert_eq!(clues.len(), 2);

        // Clues should contain string names and probabilities
        let clue_names: Vec<&str> = clues.iter().map(|(name, _)| name.as_str()).collect();
        assert!(clue_names.contains(&"spammy"));
        assert!(clue_names.contains(&"hammy"));

        // Each clue probability should be in [0, 1]
        for (_, prob) in &clues {
            assert!((0.0..=1.0).contains(prob));
        }
    }

    #[test]
    fn test_spam_prob_with_evidence_matches_spam_prob() {
        // The probability from spam_prob_with_evidence should match spam_prob
        let c = classifier_with_words(100, 100, vec![
            (b"token_a", 15, 2),
            (b"token_b", 3, 18),
        ]);
        let tokens_a = vec![b"token_a".to_vec(), b"token_b".to_vec()];
        let tokens_b = vec![b"token_a".to_vec(), b"token_b".to_vec()];

        let score = c.spam_prob(tokens_a.into_iter());
        let result = c.spam_prob_with_evidence(tokens_b.into_iter());

        assert!(
            (score - result.probability).abs() < 1e-10,
            "spam_prob={} should match spam_prob_with_evidence={}",
            score,
            result.probability
        );
    }

    #[test]
    fn test_spam_prob_with_evidence_classification_always_unsure() {
        // Classifier doesn't apply thresholds, always returns Unsure
        let c = classifier_with_words(100, 100, vec![
            (b"very_spammy", 25, 0),
        ]);
        let tokens = vec![b"very_spammy".to_vec()];
        let result = c.spam_prob_with_evidence(tokens.into_iter());
        // Even with very high spam probability, classification is Unsure
        assert!(result.probability > 0.8);
        assert_eq!(result.classification, Classification::Unsure);
    }

    #[test]
    fn test_spam_prob_with_evidence_utf8_lossy_conversion() {
        // Non-UTF8 tokens should be converted with lossy replacement
        let mut c = classifier_with_counts(100, 100);
        let non_utf8_token: Vec<u8> = vec![0xFF, 0xFE, 0x68, 0x65, 0x6C, 0x6C, 0x6F]; // invalid UTF-8 prefix + "hello"
        c.word_info.insert(
            non_utf8_token.clone(),
            WordInfo { spam_count: 20, ham_count: 0 },
        );

        let tokens = vec![non_utf8_token];
        let result = c.spam_prob_with_evidence(tokens.into_iter());

        let clues = result.clues.unwrap();
        assert_eq!(clues.len(), 1);
        // The token should be converted (with replacement characters for invalid bytes)
        assert!(clues[0].0.contains("hello"));
    }

    // ─── learn tests ─────────────────────────────────────────────────────

    #[test]
    fn test_learn_spam_increments_nspam() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec(), b"world".to_vec()];
        c.learn(tokens.into_iter(), true);
        assert_eq!(c.nspam(), 1);
        assert_eq!(c.nham(), 0);
    }

    #[test]
    fn test_learn_ham_increments_nham() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec(), b"world".to_vec()];
        c.learn(tokens.into_iter(), false);
        assert_eq!(c.nspam(), 0);
        assert_eq!(c.nham(), 1);
    }

    #[test]
    fn test_learn_spam_increments_token_spam_count() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"buy".to_vec(), b"now".to_vec()];
        c.learn(tokens.into_iter(), true);

        let buy_info = c.word_info().get(&b"buy".to_vec()).unwrap();
        assert_eq!(buy_info.spam_count, 1);
        assert_eq!(buy_info.ham_count, 0);

        let now_info = c.word_info().get(&b"now".to_vec()).unwrap();
        assert_eq!(now_info.spam_count, 1);
        assert_eq!(now_info.ham_count, 0);
    }

    #[test]
    fn test_learn_ham_increments_token_ham_count() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"meeting".to_vec(), b"agenda".to_vec()];
        c.learn(tokens.into_iter(), false);

        let meeting_info = c.word_info().get(&b"meeting".to_vec()).unwrap();
        assert_eq!(meeting_info.spam_count, 0);
        assert_eq!(meeting_info.ham_count, 1);
    }

    #[test]
    fn test_learn_deduplicates_tokens() {
        // Same token repeated should only be counted once (Requirement 4.1)
        let mut c = Classifier::with_defaults();
        let tokens = vec![
            b"hello".to_vec(),
            b"hello".to_vec(),
            b"hello".to_vec(),
        ];
        c.learn(tokens.into_iter(), true);

        let info = c.word_info().get(&b"hello".to_vec()).unwrap();
        assert_eq!(info.spam_count, 1); // counted once, not three times
        assert_eq!(c.nspam(), 1);
    }

    #[test]
    fn test_learn_clears_prob_cache() {
        let mut c = Classifier::with_defaults();
        // Populate cache
        c.probability_cached(&WordInfo { spam_count: 1, ham_count: 1 });
        assert!(!c.prob_cache.is_empty());

        let tokens = vec![b"token".to_vec()];
        c.learn(tokens.into_iter(), true);
        assert!(c.prob_cache.is_empty());
    }

    #[test]
    fn test_learn_multiple_messages() {
        let mut c = Classifier::with_defaults();

        // Train two spam messages with overlapping tokens
        let tokens1 = vec![b"buy".to_vec(), b"now".to_vec()];
        c.learn(tokens1.into_iter(), true);

        let tokens2 = vec![b"buy".to_vec(), b"cheap".to_vec()];
        c.learn(tokens2.into_iter(), true);

        assert_eq!(c.nspam(), 2);
        let buy_info = c.word_info().get(&b"buy".to_vec()).unwrap();
        assert_eq!(buy_info.spam_count, 2);

        let now_info = c.word_info().get(&b"now".to_vec()).unwrap();
        assert_eq!(now_info.spam_count, 1);
    }

    // ─── unlearn tests ───────────────────────────────────────────────────

    #[test]
    fn test_unlearn_spam_decrements_nspam() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.clone().into_iter(), true);
        assert_eq!(c.nspam(), 1);

        c.unlearn(tokens.into_iter(), true).unwrap();
        assert_eq!(c.nspam(), 0);
    }

    #[test]
    fn test_unlearn_ham_decrements_nham() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.clone().into_iter(), false);
        assert_eq!(c.nham(), 1);

        c.unlearn(tokens.into_iter(), false).unwrap();
        assert_eq!(c.nham(), 0);
    }

    #[test]
    fn test_unlearn_removes_zero_count_records() {
        // Requirement 4.7: remove token record when both counts reach zero
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.clone().into_iter(), true);
        assert!(c.word_info().contains_key(&b"hello".to_vec()));

        c.unlearn(tokens.into_iter(), true).unwrap();
        assert!(!c.word_info().contains_key(&b"hello".to_vec()));
    }

    #[test]
    fn test_unlearn_preserves_nonzero_records() {
        // If a token has counts in the other category, it stays
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.clone().into_iter(), true);
        c.learn(tokens.clone().into_iter(), false);

        // Unlearn just the spam training
        c.unlearn(tokens.into_iter(), true).unwrap();

        let info = c.word_info().get(&b"hello".to_vec()).unwrap();
        assert_eq!(info.spam_count, 0);
        assert_eq!(info.ham_count, 1); // ham count preserved
    }

    #[test]
    fn test_unlearn_spam_rejects_when_nspam_zero() {
        // Requirement 4.8: reject if count would go below zero
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        let result = c.unlearn(tokens.into_iter(), true);
        assert!(result.is_err());
        match result.unwrap_err() {
            ClassifierError::InvalidOperation(msg) => {
                assert!(msg.contains("spam"));
                assert!(msg.contains("below zero"));
            }
            other => panic!("expected InvalidOperation, got {other:?}"),
        }
    }

    #[test]
    fn test_unlearn_ham_rejects_when_nham_zero() {
        // Requirement 4.8: reject if count would go below zero
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        let result = c.unlearn(tokens.into_iter(), false);
        assert!(result.is_err());
        match result.unwrap_err() {
            ClassifierError::InvalidOperation(msg) => {
                assert!(msg.contains("ham"));
                assert!(msg.contains("below zero"));
            }
            other => panic!("expected InvalidOperation, got {other:?}"),
        }
    }

    #[test]
    fn test_unlearn_deduplicates_tokens() {
        // Repeated tokens in unlearn should only decrement once
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.into_iter(), true);

        // Train again so spam_count is 2
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.into_iter(), true);

        let info = c.word_info().get(&b"hello".to_vec()).unwrap();
        assert_eq!(info.spam_count, 2);

        // Unlearn with duplicates — should only decrement once
        let dup_tokens = vec![
            b"hello".to_vec(),
            b"hello".to_vec(),
            b"hello".to_vec(),
        ];
        c.unlearn(dup_tokens.into_iter(), true).unwrap();

        let info = c.word_info().get(&b"hello".to_vec()).unwrap();
        assert_eq!(info.spam_count, 1); // decremented once, not three times
    }

    #[test]
    fn test_unlearn_clears_prob_cache() {
        let mut c = Classifier::with_defaults();
        let tokens = vec![b"hello".to_vec()];
        c.learn(tokens.clone().into_iter(), true);

        // Populate cache
        c.probability_cached(&WordInfo { spam_count: 1, ham_count: 0 });
        assert!(!c.prob_cache.is_empty());

        c.unlearn(tokens.into_iter(), true).unwrap();
        assert!(c.prob_cache.is_empty());
    }

    #[test]
    fn test_unlearn_unknown_token_does_not_panic() {
        // Unlearning a token that doesn't exist in word_info should be safe
        let mut c = Classifier::with_defaults();
        c.nspam = 1; // set count so unlearn doesn't reject
        let tokens = vec![b"never_seen".to_vec()];
        let result = c.unlearn(tokens.into_iter(), true);
        assert!(result.is_ok());
        assert_eq!(c.nspam(), 0);
    }
}
