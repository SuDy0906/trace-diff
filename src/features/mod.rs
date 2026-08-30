//! Auto-detect website "features" (pages / common API paths) and run interactive checks.

mod auth;
mod auth_spec;
mod detect;
mod openapi_index;
mod run_report;
mod ui;
mod workflow;

pub use auth::{AuthMode, AuthProfile, AuthProfiles, RealmCredentials};
pub use detect::{discover_features, DiscoverOptions, DiscoverOutcome, LlmDiscoveryStatus};
pub use openapi_index::{templated_path_for_probe, AuthRealmHint, OpenApiIndex};
pub use run_report::{build_categorized_report, CategorizedRunReport, IssueCategory};
pub use ui::run_features_interactive;
pub use workflow::{
    detect_auth_realms, workflow_realm, FlowKind, WorkflowManifest, WorkflowScenario,
    MANIFEST_VERSION,
};

use crate::error::Result;
use crate::l7::{self, L7Config, L7Metrics};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ProbeSettings {
    pub timeout: Duration,
    pub auth: AuthProfile,
    pub cert_warn_days: i64,
}

impl ProbeSettings {
    pub fn new(timeout: Duration, auth: AuthProfile) -> Self {
        Self {
            timeout,
            auth,
            cert_warn_days: 21,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedFeature {
    pub id: String,
    pub label: String,
    pub url: String,
    pub kind: FeatureKind,
    /// Why we think this is a feature (link, sitemap, probe, …).
    pub source: String,
    /// HTTP method for probing (defaults to GET).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Multi-step scenario (LLM-generated workflow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowScenario>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeatureKind {
    Page,
    Api,
    Tls,
    Workflow,
    Meta,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeVerdict {
    Healthy,
    Reachable,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub name: String,
    pub method: String,
    pub path: String,
    pub status: Option<u16>,
    pub verdict: ProbeVerdict,
    pub message: String,
    #[serde(default)]
    pub captured_token: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureResult {
    pub feature: DetectedFeature,
    pub ok: bool,
    pub verdict: ProbeVerdict,
    pub status: Option<u16>,
    pub total_ms: f64,
    pub ttfb_ms: Option<f64>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l7: Option<L7Metrics>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureRunReport {
    pub base_url: String,
    pub discovered: usize,
    pub selected: usize,
    pub passed: usize,
    pub failed: usize,
    pub results: Vec<FeatureResult>,
}

pub async fn probe_feature(feature: &DetectedFeature, timeout: Duration) -> FeatureResult {
    let settings = ProbeSettings::new(timeout, AuthProfile::from_env(None));
    probe_feature_with_auth(feature, &settings).await
}

pub async fn probe_feature_with_auth(
    feature: &DetectedFeature,
    settings: &ProbeSettings,
) -> FeatureResult {
    if feature.kind == FeatureKind::Tls || feature.id == "tls_cert" {
        return probe_tls_canary(feature, settings).await;
    }
    if let Some(scenario) = &feature.workflow {
        return workflow::run_workflow(&feature.url, scenario, settings).await;
    }

    let method = feature.method.clone().unwrap_or_else(|| "GET".into());
    let mut cfg = L7Config {
        timeout: settings.timeout,
        method,
        max_body_bytes: 128 * 1024,
        ..L7Config::default()
    };
    if let Some(token) = settings.auth.bearer() {
        cfg.extra_headers
            .push(("Authorization".into(), format!("Bearer {token}")));
    }
    match l7::probe(&feature.url, cfg).await {
        Ok(m) => {
            let status = m.status.unwrap_or(0);
            let verdict = classify_status(status);
            let ok = verdict != ProbeVerdict::Failed;
            let message = status_message(status, verdict);
            FeatureResult {
                feature: feature.clone(),
                ok,
                verdict,
                status: m.status,
                total_ms: m.total_ms,
                ttfb_ms: m.ttfb_ms,
                message,
                l7: Some(m),
                steps: Vec::new(),
            }
        }
        Err(e) => {
            let msg = e.to_string();
            let transient = is_transient_error_message(&msg);
            let verdict = if transient {
                ProbeVerdict::Reachable
            } else {
                ProbeVerdict::Failed
            };
            FeatureResult {
                feature: feature.clone(),
                ok: verdict != ProbeVerdict::Failed,
                verdict,
                status: None,
                total_ms: 0.0,
                ttfb_ms: None,
                message: if transient {
                    format!("timeout / slow response ({msg})")
                } else {
                    msg
                },
                l7: None,
                steps: Vec::new(),
            }
        }
    }
}

async fn probe_tls_canary(feature: &DetectedFeature, settings: &ProbeSettings) -> FeatureResult {
    match l7::probe_cert(&feature.url, settings.timeout).await {
        Ok(c) => {
            let days = c.days_until_expiry.unwrap_or(i64::MIN);
            let (verdict, message) = if days == i64::MIN {
                (
                    ProbeVerdict::Reachable,
                    format!(
                        "TLS {} · handshake {:.0} ms · cert dates unread",
                        c.tls_version, c.handshake_ms
                    ),
                )
            } else if days < 0 {
                (
                    ProbeVerdict::Failed,
                    format!("TLS {} · certificate expired {}d ago", c.tls_version, -days),
                )
            } else if days < settings.cert_warn_days {
                (
                    ProbeVerdict::Reachable,
                    format!(
                        "TLS {} · cert expires in {days}d (warn < {}d)",
                        c.tls_version, settings.cert_warn_days
                    ),
                )
            } else {
                (
                    ProbeVerdict::Healthy,
                    format!(
                        "TLS {} · handshake {:.0} ms · expires in {days}d",
                        c.tls_version, c.handshake_ms
                    ),
                )
            };
            FeatureResult {
                feature: feature.clone(),
                ok: verdict != ProbeVerdict::Failed,
                verdict,
                status: None,
                total_ms: c.handshake_ms,
                ttfb_ms: Some(c.handshake_ms),
                message,
                l7: None,
                steps: Vec::new(),
            }
        }
        Err(e) => {
            let msg = format!("TLS canary failed: {e}");
            let transient = is_transient_error_message(&msg);
            let verdict = if transient {
                ProbeVerdict::Reachable
            } else {
                ProbeVerdict::Failed
            };
            FeatureResult {
                feature: feature.clone(),
                ok: verdict != ProbeVerdict::Failed,
                verdict,
                status: None,
                total_ms: 0.0,
                ttfb_ms: None,
                message: if transient {
                    format!("timeout / slow response ({msg})")
                } else {
                    msg
                },
                l7: None,
                steps: Vec::new(),
            }
        }
    }
}

pub fn classify_status(status: u16) -> ProbeVerdict {
    match status {
        200..=399 => ProbeVerdict::Healthy,
        400 | 401 | 403 | 405 | 422 => ProbeVerdict::Reachable,
        _ => ProbeVerdict::Failed,
    }
}

/// True when a probe error is a timeout / slow-response rather than a hard failure.
pub fn is_transient_probe_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || is_transient_error_message(&err.to_string())
}

pub fn is_transient_error_message(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("timed out") || m.contains("timeout") || m.contains("time out")
}

pub fn has_step_warnings(r: &FeatureResult) -> bool {
    r.steps
        .iter()
        .any(|s| s.verdict == ProbeVerdict::Reachable || s.verdict == ProbeVerdict::Failed)
}

/// Roll up per-step outcomes: mostly-pass workflows stay green when warnings are in the minority.
pub fn aggregate_workflow_verdict(outcomes: &[StepOutcome]) -> ProbeVerdict {
    if outcomes.is_empty() {
        return ProbeVerdict::Healthy;
    }
    let mut healthy = 0usize;
    let mut failed = 0usize;
    for o in outcomes {
        match o.verdict {
            ProbeVerdict::Healthy => healthy += 1,
            ProbeVerdict::Reachable => {}
            ProbeVerdict::Failed => failed += 1,
        }
    }
    if failed > 0 && healthy <= failed {
        ProbeVerdict::Failed
    } else {
        ProbeVerdict::Healthy
    }
}

pub fn status_message(status: u16, verdict: ProbeVerdict) -> String {
    match verdict {
        ProbeVerdict::Healthy => format!("HTTP {status}"),
        ProbeVerdict::Reachable => match status {
            401 | 403 => format!("HTTP {status} (auth required — route reachable)"),
            405 => format!("HTTP {status} (route exists, method/body mismatch)"),
            422 => format!("HTTP {status} (validation — route reachable, needs auth/body)"),
            400 => format!("HTTP {status} (bad request — route reachable)"),
            _ => format!("HTTP {status} (route reachable)"),
        },
        ProbeVerdict::Failed => {
            if status == 0 {
                "no HTTP status".into()
            } else {
                format!("HTTP {status} (down or not found)")
            }
        }
    }
}

pub async fn run_selected(
    base_url: &str,
    features: &[DetectedFeature],
    settings: &ProbeSettings,
) -> Result<FeatureRunReport> {
    let mut results = Vec::with_capacity(features.len());
    for f in features {
        results.push(probe_feature_with_auth(f, settings).await);
    }
    let passed = results.iter().filter(|r| r.ok).count();
    let failed = results.iter().filter(|r| !r.ok).count();
    Ok(FeatureRunReport {
        base_url: base_url.to_string(),
        discovered: features.len(),
        selected: features.len(),
        passed,
        failed,
        results,
    })
}

pub fn is_write_flow(feature: &DetectedFeature) -> bool {
    feature.source == "workflow-write"
        || feature
            .workflow
            .as_ref()
            .map(|w| w.kind == FlowKind::Write)
            .unwrap_or(false)
}

pub fn result_ttfb_ms(result: &FeatureResult) -> Option<f64> {
    result
        .l7
        .as_ref()
        .and_then(|m| m.ttfb_ms)
        .or(result.ttfb_ms)
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    fn step(name: &str, verdict: ProbeVerdict) -> StepOutcome {
        StepOutcome {
            name: name.into(),
            method: "GET".into(),
            path: "/".into(),
            status: None,
            verdict,
            message: String::new(),
            captured_token: false,
        }
    }

    #[test]
    fn transient_error_message_detects_timeout() {
        assert!(is_transient_error_message("TCP connect timed out"));
        assert!(is_transient_error_message("operation timed out"));
        assert!(!is_transient_error_message("connection refused"));
    }

    #[test]
    fn aggregate_mixed_timeout_steps_stays_healthy() {
        let outcomes = vec![
            step("a", ProbeVerdict::Healthy),
            step("b", ProbeVerdict::Healthy),
            step("c", ProbeVerdict::Reachable),
        ];
        assert_eq!(aggregate_workflow_verdict(&outcomes), ProbeVerdict::Healthy);
    }

    #[test]
    fn aggregate_majority_failures_stays_failed() {
        let outcomes = vec![
            step("a", ProbeVerdict::Failed),
            step("b", ProbeVerdict::Failed),
            step("c", ProbeVerdict::Healthy),
        ];
        assert_eq!(aggregate_workflow_verdict(&outcomes), ProbeVerdict::Failed);
    }

    #[test]
    fn aggregate_minority_failures_stays_healthy_with_warnings() {
        let outcomes = vec![
            step("a", ProbeVerdict::Healthy),
            step("b", ProbeVerdict::Healthy),
            step("c", ProbeVerdict::Healthy),
            step("d", ProbeVerdict::Failed),
        ];
        assert_eq!(aggregate_workflow_verdict(&outcomes), ProbeVerdict::Healthy);
    }
}
