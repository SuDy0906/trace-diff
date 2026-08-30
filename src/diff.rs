//! Regression / topology diff engine.

use crate::l7::L7Metrics;
use crate::stats::pct_delta;
use crate::store::StoredRun;
use crate::traceroute::TraceResult;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    pub baseline_name: Option<String>,
    pub baseline_run_id: String,
    pub current_run_id: String,
    pub l7: Option<L7Diff>,
    pub hops: Vec<HopDiff>,
    pub topology: TopologyDiff,
    pub regressions: Vec<Regression>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L7Diff {
    pub dns_delta_pct: Option<f64>,
    pub tcp_delta_pct: Option<f64>,
    pub tls_delta_pct: Option<f64>,
    pub ttfb_delta_pct: Option<f64>,
    pub transfer_delta_pct: Option<f64>,
    pub total_delta_pct: Option<f64>,
    pub current: L7Metrics,
    pub baseline: L7Metrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HopDiff {
    pub ttl: u8,
    pub current_address: Option<IpAddr>,
    pub baseline_address: Option<IpAddr>,
    pub rtt_delta_pct: Option<f64>,
    pub current_p50_ms: Option<f64>,
    pub baseline_p50_ms: Option<f64>,
    pub address_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TopologyDiff {
    pub added: Vec<IpAddr>,
    pub dropped: Vec<IpAddr>,
    pub reordered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regression {
    pub severity: Severity,
    pub metric: String,
    pub message: String,
    pub delta_pct: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

/// Thresholds (percent) used to classify regressions.
#[derive(Debug, Clone)]
pub struct DiffThresholds {
    pub warn_pct: f64,
    pub critical_pct: f64,
}

impl Default for DiffThresholds {
    fn default() -> Self {
        Self {
            warn_pct: 20.0,
            critical_pct: 50.0,
        }
    }
}

pub fn diff_runs(
    baseline: &StoredRun,
    current: &StoredRun,
    baseline_name: Option<String>,
    thresholds: &DiffThresholds,
) -> DiffReport {
    let l7 = match (&baseline.l7, &current.l7) {
        (Some(b), Some(c)) => Some(diff_l7(b, c)),
        _ => None,
    };

    let (hops, topology) = match (&baseline.trace, &current.trace) {
        (Some(b), Some(c)) => diff_hops(b, c),
        _ => (Vec::new(), TopologyDiff::default()),
    };

    let mut regressions = Vec::new();
    if let Some(ref d) = l7 {
        push_metric_regression(&mut regressions, "ttfb", d.ttfb_delta_pct, thresholds);
        push_metric_regression(
            &mut regressions,
            "tcp_handshake",
            d.tcp_delta_pct,
            thresholds,
        );
        push_metric_regression(&mut regressions, "dns", d.dns_delta_pct, thresholds);
        push_metric_regression(&mut regressions, "tls", d.tls_delta_pct, thresholds);
        push_metric_regression(&mut regressions, "total", d.total_delta_pct, thresholds);
    }

    for h in &hops {
        if h.address_changed {
            regressions.push(Regression {
                severity: Severity::Warn,
                metric: format!("hop_{}", h.ttl),
                message: format!(
                    "hop {} address changed: {:?} → {:?}",
                    h.ttl, h.baseline_address, h.current_address
                ),
                delta_pct: None,
            });
        }
        if let Some(delta) = h.rtt_delta_pct {
            if delta >= thresholds.warn_pct {
                push_metric_regression(
                    &mut regressions,
                    &format!("hop_{}_rtt", h.ttl),
                    Some(delta),
                    thresholds,
                );
            }
        }
    }

    if !topology.added.is_empty() || !topology.dropped.is_empty() || topology.reordered {
        regressions.push(Regression {
            severity: Severity::Warn,
            metric: "topology".into(),
            message: format!(
                "route topology changed (added={}, dropped={}, reordered={})",
                topology.added.len(),
                topology.dropped.len(),
                topology.reordered
            ),
            delta_pct: None,
        });
    }

    DiffReport {
        baseline_name,
        baseline_run_id: baseline.id.clone(),
        current_run_id: current.id.clone(),
        l7,
        hops,
        topology,
        regressions,
    }
}

fn diff_l7(baseline: &L7Metrics, current: &L7Metrics) -> L7Diff {
    L7Diff {
        dns_delta_pct: opt_delta(current.dns_ms, baseline.dns_ms),
        tcp_delta_pct: opt_delta(current.tcp_ms, baseline.tcp_ms),
        tls_delta_pct: opt_delta(current.tls_ms, baseline.tls_ms),
        ttfb_delta_pct: opt_delta(current.ttfb_ms, baseline.ttfb_ms),
        transfer_delta_pct: opt_delta(current.transfer_ms, baseline.transfer_ms),
        total_delta_pct: pct_delta(current.total_ms, baseline.total_ms),
        current: current.clone(),
        baseline: baseline.clone(),
    }
}

fn opt_delta(current: Option<f64>, baseline: Option<f64>) -> Option<f64> {
    match (current, baseline) {
        (Some(c), Some(b)) => pct_delta(c, b),
        _ => None,
    }
}

fn diff_hops(baseline: &TraceResult, current: &TraceResult) -> (Vec<HopDiff>, TopologyDiff) {
    let max_ttl = baseline.hops.len().max(current.hops.len()).min(64);
    let mut hops = Vec::new();
    for i in 0..max_ttl {
        let b = baseline.hops.get(i);
        let c = current.hops.get(i);
        let ttl = (i as u8) + 1;
        let b_addr = b.and_then(|h| h.address);
        let c_addr = c.and_then(|h| h.address);
        let b_p50 = b.and_then(|h| h.metrics.p50_ms);
        let c_p50 = c.and_then(|h| h.metrics.p50_ms);
        hops.push(HopDiff {
            ttl: b.map(|h| h.ttl).or_else(|| c.map(|h| h.ttl)).unwrap_or(ttl),
            current_address: c_addr,
            baseline_address: b_addr,
            rtt_delta_pct: match (c_p50, b_p50) {
                (Some(c), Some(b)) => pct_delta(c, b),
                _ => None,
            },
            current_p50_ms: c_p50,
            baseline_p50_ms: b_p50,
            address_changed: b_addr != c_addr && (b_addr.is_some() || c_addr.is_some()),
        });
    }

    let b_path: Vec<IpAddr> = baseline.hops.iter().filter_map(|h| h.address).collect();
    let c_path: Vec<IpAddr> = current.hops.iter().filter_map(|h| h.address).collect();

    let added: Vec<IpAddr> = c_path
        .iter()
        .filter(|ip| !b_path.contains(ip))
        .copied()
        .collect();
    let dropped: Vec<IpAddr> = b_path
        .iter()
        .filter(|ip| !c_path.contains(ip))
        .copied()
        .collect();

    // Reordered if shared addresses appear in different relative order.
    let shared: Vec<_> = b_path
        .iter()
        .filter(|ip| c_path.contains(ip))
        .copied()
        .collect();
    let c_shared: Vec<_> = c_path
        .iter()
        .filter(|ip| b_path.contains(ip))
        .copied()
        .collect();
    let reordered = shared != c_shared;

    (
        hops,
        TopologyDiff {
            added,
            dropped,
            reordered,
        },
    )
}

fn push_metric_regression(
    out: &mut Vec<Regression>,
    metric: &str,
    delta: Option<f64>,
    thresholds: &DiffThresholds,
) {
    let Some(d) = delta else { return };
    if d < thresholds.warn_pct {
        return;
    }
    let severity = if d >= thresholds.critical_pct {
        Severity::Critical
    } else {
        Severity::Warn
    };
    out.push(Regression {
        severity,
        metric: metric.into(),
        message: format!("{metric} increased by {d:.1}% vs baseline"),
        delta_pct: Some(d),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_l7(ttfb: f64, total: f64) -> L7Metrics {
        L7Metrics {
            url: "https://example.com".into(),
            resolved_ip: Some("1.2.3.4".into()),
            status: Some(200),
            dns_ms: Some(10.0),
            tcp_ms: Some(20.0),
            tls_ms: Some(30.0),
            ttfb_ms: Some(ttfb),
            transfer_ms: Some(5.0),
            total_ms: total,
            bytes_read: 100,
            measured_at: Utc::now(),
        }
    }

    #[test]
    fn detects_ttfb_regression() {
        let baseline = StoredRun {
            id: "b".into(),
            target: "https://example.com".into(),
            created_at: Utc::now(),
            resolved_ip: None,
            reached: true,
            trace: None,
            l7: Some(sample_l7(100.0, 200.0)),
            meta: None,
        };
        let current = StoredRun {
            id: "c".into(),
            target: "https://example.com".into(),
            created_at: Utc::now(),
            resolved_ip: None,
            reached: true,
            trace: None,
            l7: Some(sample_l7(160.0, 280.0)),
            meta: None,
        };
        let report = diff_runs(
            &baseline,
            &current,
            Some("staging".into()),
            &DiffThresholds::default(),
        );
        assert!(report
            .regressions
            .iter()
            .any(|r| r.metric == "ttfb" && r.severity == Severity::Critical));
        assert!((report.l7.as_ref().unwrap().ttfb_delta_pct.unwrap() - 60.0).abs() < 1e-6);
    }
}
