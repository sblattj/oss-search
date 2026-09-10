//! Signal normalizers: min-max, the npms.io Bezier scoring curve, and
//! saturating log-scale normalization for heavy-tailed counts.

use serde::{Deserialize, Serialize};

/// Corpus-wide aggregation of a raw signal used to normalize one value.
///
/// Mirrors npms-analyzer's per-evaluation aggregation (min / max / mean over
/// the corpus). npms actually uses a weighted truncated mean; a plain mean is
/// our documented approximation — the curve shape, not the mean estimator,
/// is what carries the scoring semantics.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aggregation {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

impl Aggregation {
    /// Build an aggregation from a corpus slice. Returns `None` for an empty
    /// slice (no basis for normalization).
    pub fn from_values(values: &[f64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        Some(Self { min, max, mean })
    }
}

/// Min-max normalize `value` into `[0, 1]` against `[min, max]`, clamping
/// out-of-range values. Degenerate range (`max <= min`) returns the neutral
/// 0.5 instead of NaN.
pub fn min_max(value: f64, min: f64, max: f64) -> f64 {
    if max <= min {
        return 0.5;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// Saturating log-scale normalization for heavy-tailed counts (stars,
/// dependents, HN mentions): `ln(1+v) / ln(1+soft_cap)`, clamped to `[0, 1]`.
///
/// Counts in OSS ecosystems are scale-free — linear scaling lets one
/// mega-repo flatten everyone else to zero; a log scale with a soft cap
/// (the value that maps to 1.0) preserves ordering while saturating the tail.
pub fn log_saturate(value: f64, soft_cap: f64) -> f64 {
    debug_assert!(soft_cap > 0.0, "soft_cap must be positive");
    if value <= 0.0 {
        return 0.0;
    }
    (value.ln_1p() / soft_cap.ln_1p()).clamp(0.0, 1.0)
}

/// The npms.io scoring curve: a cubic Bezier with control points
/// `(0, 0), (mean, 0.75), (mean, 0.75), (1, 1)`, evaluated at the parameter
/// whose X equals `normalized`.
///
/// This is npms-analyzer's documented formula (lib/scoring/normalize.js):
/// after min-max normalizing a package's evaluation against the corpus, the
/// score is the Y of this curve at the package's X. The doubled control
/// point at `(mean, 0.75)` makes the curve rise steeply then flatten —
/// values at the corpus mean score ≈ 0.69–0.75, and the curve saturates
/// well before the corpus max, which de-inflates the mega-hub tail.
///
/// Parametrically, with `m = mean`:
///
/// ```text
/// X(t) = 3·m·t·(1−t) + t³
/// Y(t) = 2.25·t·(1−t) + t³        (= X(t) with m → 0.75)
/// ```
///
/// `X(t)` is strictly increasing on `[0, 1]` for `m ∈ (0, 1)`
/// (`X'(t) = 3m(1−2t) + 3t²` is minimized at `t = m` with value
/// `3m(1−m) ≥ 0`), so `t` is recovered from `x` by bisection — exact to
/// floating-point tolerance, no closed-form cubic solve needed.
pub fn npms_bezier_score(normalized: f64, mean: f64) -> f64 {
    let m = mean.clamp(0.0, 1.0);
    let x = normalized.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        let xm = 3.0 * m * mid * (1.0 - mid) + mid * mid * mid;
        if xm < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t = 0.5 * (lo + hi);
    2.25 * t * (1.0 - t) + t * t * t
}

/// Full npms-style score for one raw value against a corpus aggregation:
/// min-max the value AND the corpus mean, then run the Bezier.
///
/// (npms normalizes the mean with the same min-max bounds, so the curve's
/// control point sits at the *normalized* corpus mean.)
pub fn npms_score(value: f64, agg: &Aggregation) -> f64 {
    if agg.max <= agg.min {
        return 0.5;
    }
    let norm_mean = min_max(agg.mean, agg.min, agg.max);
    npms_bezier_score(min_max(value, agg.min, agg.max), norm_mean)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn bezier_endpoints_are_exact() {
        assert!(approx(npms_bezier_score(0.0, 0.5), 0.0));
        assert!(approx(npms_bezier_score(1.0, 0.5), 1.0));
        assert!(approx(npms_bezier_score(0.0, 0.3), 0.0));
        assert!(approx(npms_bezier_score(1.0, 0.3), 1.0));
    }

    #[test]
    fn bezier_midpoint_hand_computed() {
        // mean = 0.5, x = 0.5: X(t) = 1.5t(1-t) + t^3 = 0.5 is solved exactly
        // by t = 0.5 (1.5*0.25 + 0.125 = 0.5). Y(0.5) = 2.25*0.25 + 0.125
        // = 0.5625 + 0.125 = 0.6875. A package exactly at the corpus mean
        // scores 0.6875 — the curve sits well above the diagonal there.
        assert!(approx(npms_bezier_score(0.5, 0.5), 0.6875));
    }

    #[test]
    fn bezier_off_diagonal_hand_computed() {
        // mean = 0.5, x = 0.248: X(t) = 1.5t(1-t) + t^3 = 0.248 is solved
        // exactly by t = 0.2 (1.5*0.2*0.8 + 0.008 = 0.248).
        // Y(0.2) = 2.25*0.2*0.8 + 0.008 = 0.36 + 0.008 = 0.368.
        assert!(approx(npms_bezier_score(0.248, 0.5), 0.368));
    }

    #[test]
    fn bezier_is_monotonic_and_bounded() {
        for &m in &[0.1f64, 0.3, 0.5, 0.7, 0.9] {
            let mut prev = -1.0;
            for i in 0..=20 {
                let x = i as f64 / 20.0;
                let y = npms_bezier_score(x, m);
                assert!((0.0..=1.0).contains(&y), "y out of range: {y}");
                assert!(y >= prev, "not monotonic at m={m}, x={x}");
                prev = y;
            }
        }
    }

    #[test]
    fn min_max_basic_clamps_and_degenerate() {
        assert!(approx(min_max(5.0, 0.0, 10.0), 0.5));
        assert!(approx(min_max(-3.0, 0.0, 10.0), 0.0));
        assert!(approx(min_max(99.0, 0.0, 10.0), 1.0));
        assert!(approx(min_max(7.0, 7.0, 7.0), 0.5));
    }

    #[test]
    fn log_saturate_endpoints_and_midpoint() {
        assert!(approx(log_saturate(0.0, 100.0), 0.0));
        assert!(approx(log_saturate(100.0, 100.0), 1.0));
        assert!(approx(log_saturate(100_000.0, 100.0), 1.0));
        // ln(1+v)/ln(1+cap) = 0.5 exactly at v = sqrt(1+cap) - 1.
        let cap = 99.0f64;
        let mid = (1.0 + cap).sqrt() - 1.0;
        assert!(approx(log_saturate(mid, cap), 0.5));
    }

    #[test]
    fn npms_score_uses_normalized_mean_and_orders_with_value() {
        let values = [1.0, 5.0, 20.0, 100.0, 500.0];
        let agg = Aggregation::from_values(&values).expect("non-empty");
        let scores: Vec<f64> = values.iter().map(|v| npms_score(*v, &agg)).collect();
        for w in scores.windows(2) {
            assert!(w[1] > w[0], "score must increase with value");
        }
        assert!(scores.iter().all(|s| (0.0..=1.0).contains(s)));
        // The corpus-mean package lands near 0.7 on the curve.
        let mean_score = npms_score(agg.mean, &agg);
        assert!(
            mean_score > 0.6 && mean_score < 0.8,
            "mean score {mean_score}"
        );
    }

    #[test]
    fn aggregation_from_values_basics() {
        assert!(Aggregation::from_values(&[]).is_none());
        let agg = Aggregation::from_values(&[2.0, 4.0, 6.0]).expect("non-empty");
        assert!(approx(agg.min, 2.0));
        assert!(approx(agg.max, 6.0));
        assert!(approx(agg.mean, 4.0));
    }
}
