#![warn(clippy::pedantic)]
// ── Pedantic allow-list (documented exceptions) ──────────────────────────────
// 6. cast_possible_truncation: Controlled numeric casts (u64→f64 for counts,
//    usize→u32 for message counts) are acceptable since values stay small.
#![allow(clippy::cast_possible_truncation)]
// 7. cast_sign_loss: Token length rounding uses f64→usize where values are
//    always non-negative by construction.
#![allow(clippy::cast_sign_loss)]
// 8. cast_precision_loss: u64→f64 casts for nspam/nham counts—precision loss
//    is negligible for realistic corpus sizes (< 2^52 messages).
#![allow(clippy::cast_precision_loss)]
// trivially_copy_pass_by_ref: WordInfo is 8 bytes (Copy), but we pass by
// reference for API consistency with the Python implementation's conventions.
#![allow(clippy::trivially_copy_pass_by_ref)]

//! `SpamBayes` Core - Pure Bayesian classifier and tokenizer logic.
//!
//! This crate contains the domain layer with zero Windows dependencies.
//! It implements the chi-squared combining classifier and email tokenizer.

pub mod chi2;
pub mod classifier;
pub mod tokenizer;
pub mod word_info;

use thiserror::Error;

// ─── WordInfo ────────────────────────────────────────────────────────────────

/// Per-token spam and ham occurrence counts.
///
/// Each token is counted at most once per message regardless of how many
/// times it appears in that message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WordInfo {
    /// Number of spam messages this token appeared in.
    pub spam_count: u32,
    /// Number of ham messages this token appeared in.
    pub ham_count: u32,
}

// ─── Classification ──────────────────────────────────────────────────────────

/// Message classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Classification {
    /// Legitimate (non-spam) message.
    Ham,
    /// Junk / unwanted message.
    Spam,
    /// Classifier cannot confidently decide.
    Unsure,
}

// ─── ScoreResult ─────────────────────────────────────────────────────────────

/// Result of scoring a message, including probability, classification,
/// and optionally the contributing clues.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreResult {
    /// Spam probability in the range `0.0..=1.0`.
    pub probability: f64,
    /// The classification derived from the probability and thresholds.
    pub classification: Classification,
    /// Optional list of (token, probability) clues used in scoring.
    pub clues: Option<Vec<(String, f64)>>,
}

// ─── ClassifierConfig ────────────────────────────────────────────────────────

/// Configuration parameters for the Bayesian classifier.
///
/// Default values match the Python `SpamBayes` implementation.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierConfig {
    /// Strength given to the unknown-word probability (Robinson's `S`).
    /// Default: `0.45`.
    pub unknown_word_strength: f64,
    /// Probability assigned to tokens never seen in training (Robinson's `x`).
    /// Default: `0.5`.
    pub unknown_word_prob: f64,
    /// Maximum number of most-significant tokens used for scoring.
    /// Default: `150`.
    pub max_discriminators: usize,
    /// Minimum distance from 0.5 for a token probability to be considered
    /// significant. Default: `0.1`.
    pub minimum_prob_strength: f64,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            unknown_word_strength: 0.45,
            unknown_word_prob: 0.5,
            max_discriminators: 150,
            minimum_prob_strength: 0.1,
        }
    }
}

// ─── ClassifierError ─────────────────────────────────────────────────────────

/// Errors that can occur during classifier operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClassifierError {
    /// An operation would cause a count to go below zero (e.g., untraining
    /// a message that was never trained).
    #[error("invalid operation: {0}")]
    InvalidOperation(String),

    /// The input provided to the classifier was empty (no tokens to score).
    #[error("empty input: no tokens provided")]
    EmptyInput,

    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    InternalError(String),
}
