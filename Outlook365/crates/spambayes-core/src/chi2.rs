//! Chi-squared cumulative distribution and score combining.
//!
//! This module ports the Python `SpamBayes` `chi2.py` logic to Rust.
//! It provides:
//! - [`chi2_q`]: P(chi-squared >= x2) with v degrees of freedom
//! - [`chi2_combine`]: Chi-squared score combining for the classifier

/// Decompose a floating-point number into a normalized fraction and exponent.
///
/// Returns `(frac, exp)` such that `x == frac * 2^exp`, where `0.5 <= |frac| < 1.0`.
/// Special cases: returns `(x, 0)` for 0.0, NaN, and infinity.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || x.is_nan() || x.is_infinite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7FF) as i32 - 1022;
    let frac = f64::from_bits((bits & 0x800F_FFFF_FFFF_FFFF) | 0x3FE0_0000_0000_0000);
    (frac, exp)
}

/// Compute P(chi-squared >= x2) with `v` degrees of freedom.
///
/// `v` must be even. Returns a value clamped to `[0.0, 1.0]`.
///
/// This is a direct port of the Python `chi2Q` function from `SpamBayes`.
/// The algorithm computes the survival function of the chi-squared distribution
/// using a series expansion.
#[must_use]
pub fn chi2_q(x2: f64, v: u32) -> f64 {
    debug_assert!(v & 1 == 0, "v must be even, got {v}");

    let m = x2 / 2.0;
    let mut sum = (-m).exp();
    let mut term = sum;

    for i in 1..(v / 2) {
        term *= m / f64::from(i);
        sum += term;
    }

    // With small x2 and large v, accumulated roundoff error plus error in exp()
    // can cause this to spill a few ULP above 1.0. Clamp to 1.0 max.
    sum.min(1.0)
}

/// Chi-squared score combining for the classifier.
///
/// Takes a slice of token probabilities (each in `0.0..=1.0`) and returns
/// `(spam_indicator, ham_indicator, combined_score)` where:
/// - `spam_indicator` (S): probability that the message is spam
/// - `ham_indicator` (H): probability that the message is ham
/// - `combined_score`: `(S - H + 1.0) / 2.0`, ranging from 0.0 (ham) to 1.0 (spam)
///
/// Uses frexp-style rescaling to prevent floating-point underflow when
/// computing products of many small probabilities.
///
/// Returns `(0.5, 0.5, 0.5)` for an empty probability slice (no evidence).
#[must_use]
pub fn chi2_combine(probs: &[f64]) -> (f64, f64, f64) {
    if probs.is_empty() {
        return (0.5, 0.5, 0.5);
    }

    let mut h: f64 = 1.0; // product of p_i
    let mut s: f64 = 1.0; // product of (1 - p_i)
    let mut h_exp: i32 = 0;
    let mut s_exp: i32 = 0;

    for &p in probs {
        s *= 1.0 - p;
        h *= p;

        // Rescale S if it gets dangerously small to prevent underflow
        if s < 1e-200 {
            let (frac, e) = frexp(s);
            s = frac;
            s_exp += e;
        }

        // Rescale H if it gets dangerously small to prevent underflow
        if h < 1e-200 {
            let (frac, e) = frexp(h);
            h = frac;
            h_exp += e;
        }
    }

    let ln2: f64 = 2.0_f64.ln();
    let s_ln = s.ln() + f64::from(s_exp) * ln2;
    let h_ln = h.ln() + f64::from(h_exp) * ln2;

    let n = probs.len() as u32;

    let s_stat = 1.0 - chi2_q(-2.0 * s_ln, 2 * n);
    let h_stat = 1.0 - chi2_q(-2.0 * h_ln, 2 * n);
    let score = f64::midpoint(s_stat - h_stat, 1.0);

    (s_stat, h_stat, score)
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Test assertions comparing exact f64 values
mod tests {
    use super::*;

    // ─── frexp tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_frexp_zero() {
        let (frac, exp) = frexp(0.0);
        assert_eq!(frac, 0.0);
        assert_eq!(exp, 0);
    }

    #[test]
    fn test_frexp_one() {
        let (frac, exp) = frexp(1.0);
        // 1.0 == 0.5 * 2^1
        assert!((frac - 0.5).abs() < 1e-15);
        assert_eq!(exp, 1);
    }

    #[test]
    fn test_frexp_two() {
        let (frac, exp) = frexp(2.0);
        // 2.0 == 0.5 * 2^2
        assert!((frac - 0.5).abs() < 1e-15);
        assert_eq!(exp, 2);
    }

    #[test]
    fn test_frexp_negative() {
        let (frac, exp) = frexp(-4.0);
        // -4.0 == -0.5 * 2^3
        assert!((frac - (-0.5)).abs() < 1e-15);
        assert_eq!(exp, 3);
    }

    #[test]
    fn test_frexp_small() {
        let (frac, exp) = frexp(0.25);
        // 0.25 == 0.5 * 2^(-1)
        assert!((frac - 0.5).abs() < 1e-15);
        assert_eq!(exp, -1);
    }

    #[test]
    fn test_frexp_nan() {
        let (frac, exp) = frexp(f64::NAN);
        assert!(frac.is_nan());
        assert_eq!(exp, 0);
    }

    #[test]
    fn test_frexp_infinity() {
        let (frac, exp) = frexp(f64::INFINITY);
        assert!(frac.is_infinite());
        assert_eq!(exp, 0);
    }

    // ─── chi2_q tests ────────────────────────────────────────────────────────

    #[test]
    fn test_chi2_q_zero_x2() {
        // With x2=0, exp(-0)=1, and the sum should be 1.0
        let result = chi2_q(0.0, 2);
        assert!((result - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_chi2_q_large_x2() {
        // With very large x2, probability should be near 0
        let result = chi2_q(100.0, 2);
        assert!(result < 1e-10);
    }

    #[test]
    fn test_chi2_q_v2() {
        // chi2Q(x2, 2) = exp(-x2/2) since the loop runs 0 times (range(1,1) is empty)
        let x2 = 4.0;
        let expected = (-2.0_f64).exp(); // exp(-4/2) = exp(-2)
        let result = chi2_q(x2, 2);
        assert!((result - expected).abs() < 1e-15);
    }

    #[test]
    fn test_chi2_q_v4() {
        // chi2Q(x2, 4): sum = exp(-m) + exp(-m)*m = exp(-m)*(1+m) where m=x2/2
        let x2 = 6.0_f64;
        let m = 3.0_f64;
        let expected = (-m).exp() * (1.0 + m);
        let result = chi2_q(x2, 4);
        assert!((result - expected).abs() < 1e-15);
    }

    #[test]
    fn test_chi2_q_never_exceeds_one() {
        // Even with small x2 and large v, result should not exceed 1.0
        let result = chi2_q(0.01, 300);
        assert!(result <= 1.0);
    }

    #[test]
    fn test_chi2_q_known_value() {
        // chi2Q(10, 10): computed analytically or from Python reference
        // Python: chi2Q(10.0, 10) ≈ 0.4404932985...
        let result = chi2_q(10.0, 10);
        assert!((result - 0.4404_9329).abs() < 1e-5);
    }

    // ─── chi2_combine tests ──────────────────────────────────────────────────

    #[test]
    fn test_chi2_combine_empty() {
        let result = chi2_combine(&[]);
        assert_eq!(result, (0.5, 0.5, 0.5));
    }

    #[test]
    fn test_chi2_combine_neutral() {
        // All probabilities at 0.5 should yield a neutral score near 0.5
        let probs = vec![0.5; 10];
        let (s, h, score) = chi2_combine(&probs);
        assert!((score - 0.5).abs() < 0.01, "score={score}");
        // S and H should be roughly equal for balanced input
        assert!((s - h).abs() < 0.01, "s={s}, h={h}");
    }

    #[test]
    fn test_chi2_combine_spammy() {
        // High probabilities => strong spam signal
        let probs = vec![0.99; 20];
        let (s, h, score) = chi2_combine(&probs);
        assert!(score > 0.9, "score={score}");
        assert!(s > 0.9, "s={s}");
        assert!(h < 0.1, "h={h}");
    }

    #[test]
    fn test_chi2_combine_hammy() {
        // Low probabilities => strong ham signal
        let probs = vec![0.01; 20];
        let (s, h, score) = chi2_combine(&probs);
        assert!(score < 0.1, "score={score}");
        assert!(s < 0.1, "s={s}");
        assert!(h > 0.9, "h={h}");
    }

    #[test]
    fn test_chi2_combine_single_prob() {
        // Single probability of 0.9
        let (s, h, score) = chi2_combine(&[0.9]);
        // S = 1 - chi2Q(-2*ln(0.1), 2) = 1 - exp(ln(0.1)) = 1 - 0.1 = 0.9
        // H = 1 - chi2Q(-2*ln(0.9), 2) = 1 - exp(ln(0.9)) = 1 - 0.9 = 0.1
        // For v=2: chi2Q(x,2) = exp(-x/2), so:
        //   S = 1 - exp(-(-2*ln(0.1))/2) = 1 - exp(ln(0.1)) = 1 - 0.1 = 0.9
        //   H = 1 - exp(-(-2*ln(0.9))/2) = 1 - exp(ln(0.9)) = 1 - 0.9 = 0.1
        assert!((s - 0.9).abs() < 1e-10, "s={s}");
        assert!((h - 0.1).abs() < 1e-10, "h={h}");
        assert!((score - 0.9).abs() < 1e-10, "score={score}");
    }

    #[test]
    fn test_chi2_combine_underflow_prevention() {
        // Many extreme probabilities that would underflow without frexp rescaling
        let probs = vec![0.0001; 100];
        let (s, h, score) = chi2_combine(&probs);
        // Should not panic or return NaN
        assert!(!s.is_nan(), "s is NaN");
        assert!(!h.is_nan(), "h is NaN");
        assert!(!score.is_nan(), "score is NaN");
        // Very low probs => strong ham signal
        assert!(score < 0.1, "score={score}");
    }

    #[test]
    fn test_chi2_combine_score_range() {
        // Score should always be in [0, 1] for valid inputs
        let test_cases: Vec<Vec<f64>> = vec![
            vec![0.1, 0.2, 0.3],
            vec![0.7, 0.8, 0.9],
            vec![0.5, 0.5, 0.5, 0.5, 0.5],
            vec![0.01, 0.99],
        ];

        for probs in &test_cases {
            let (s, h, score) = chi2_combine(probs);
            assert!(
                (0.0..=1.0).contains(&score),
                "score {score} out of range for {probs:?}"
            );
            assert!(
                (0.0..=1.0).contains(&s),
                "s {s} out of range for {probs:?}"
            );
            assert!(
                (0.0..=1.0).contains(&h),
                "h {h} out of range for {probs:?}"
            );
        }
    }
}
