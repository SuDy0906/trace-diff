//! Statistical helpers: percentiles, jitter (RFC 3550), loss rate.

/// Compute the p-th percentile (0..=100) of a sorted-or-unsorted slice of f64 values (ms).
pub fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (p / 100.0) * (sorted.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        Some(sorted[lo])
    } else {
        let w = rank - lo as f64;
        Some(sorted[lo] * (1.0 - w) + sorted[hi] * w)
    }
}

/// Arithmetic mean.
pub fn mean(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    Some(samples.iter().sum::<f64>() / samples.len() as f64)
}

/// Minimum value.
pub fn min(samples: &[f64]) -> Option<f64> {
    samples
        .iter()
        .copied()
        .reduce(|a, b| if a < b { a } else { b })
}

/// Maximum value.
pub fn max(samples: &[f64]) -> Option<f64> {
    samples
        .iter()
        .copied()
        .reduce(|a, b| if a > b { a } else { b })
}

/// RFC 3550 interarrival jitter estimate (simplified one-pass over RTT samples).
/// Uses successive RTT differences as a stand-in for interarrival variance.
pub fn jitter_rfc3550(rtts_ms: &[f64]) -> f64 {
    if rtts_ms.len() < 2 {
        return 0.0;
    }
    let mut j = 0.0_f64;
    for w in rtts_ms.windows(2) {
        let d = (w[1] - w[0]).abs();
        j += (d - j) / 16.0;
    }
    j
}

/// Packet loss percentage given sent/received counts.
pub fn loss_pct(sent: u32, recv: u32) -> f64 {
    if sent == 0 {
        return 0.0;
    }
    ((sent.saturating_sub(recv)) as f64 / sent as f64) * 100.0
}

/// Percentage delta: ((current - baseline) / baseline) * 100.
/// Returns `None` if baseline is zero or non-finite.
pub fn pct_delta(current: f64, baseline: f64) -> Option<f64> {
    if !baseline.is_finite() || baseline.abs() < f64::EPSILON {
        return None;
    }
    Some(((current - baseline) / baseline) * 100.0)
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LatencySummary {
    pub min_ms: Option<f64>,
    pub avg_ms: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub jitter_ms: f64,
    pub sent: u32,
    pub recv: u32,
    pub loss_pct: f64,
}

impl LatencySummary {
    pub fn from_samples(samples: &[f64], sent: u32) -> Self {
        let recv = samples.len() as u32;
        Self {
            min_ms: min(samples),
            avg_ms: mean(samples),
            p50_ms: percentile(samples, 50.0),
            p95_ms: percentile(samples, 95.0),
            max_ms: max(samples),
            jitter_ms: jitter_rfc3550(samples),
            sent,
            recv,
            loss_pct: loss_pct(sent, recv),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn percentile_known() {
        let s = [10.0, 20.0, 30.0, 40.0, 50.0];
        assert!((percentile(&s, 50.0).unwrap() - 30.0).abs() < 1e-9);
        assert!((percentile(&s, 0.0).unwrap() - 10.0).abs() < 1e-9);
        assert!((percentile(&s, 100.0).unwrap() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn loss_and_delta() {
        assert!((loss_pct(10, 8) - 20.0).abs() < 1e-9);
        assert!((pct_delta(120.0, 100.0).unwrap() - 20.0).abs() < 1e-9);
        assert!(pct_delta(10.0, 0.0).is_none());
    }

    #[test]
    fn empty_samples() {
        assert!(percentile(&[], 50.0).is_none());
        assert!(mean(&[]).is_none());
        assert_eq!(jitter_rfc3550(&[]), 0.0);
    }

    proptest! {
        #[test]
        fn percentile_in_range(mut xs in prop::collection::vec(0.0f64..10_000.0, 1..64), p in 0.0f64..=100.0) {
            let v = percentile(&xs, p).unwrap();
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            prop_assert!(v >= xs[0] - 1e-9);
            prop_assert!(v <= *xs.last().unwrap() + 1e-9);
        }
    }
}
