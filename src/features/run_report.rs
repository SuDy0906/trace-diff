//! Categorized post-run report: severe vs auth vs compatibility vs performance.

use super::{
    is_transient_error_message, is_write_flow, FeatureResult, FeatureRunReport, ProbeVerdict,
    StepOutcome,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueCategory {
    Severe,
    Auth,
    Compatibility,
    Performance,
}

impl IssueCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Severe => "Severe",
            Self::Auth => "Auth / access",
            Self::Compatibility => "Compatibility / probe",
            Self::Performance => "Performance",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Severe => "Likely real service failures — investigate backend health",
            Self::Auth => "Missing or rejected credentials — fix auth profile",
            Self::Compatibility => {
                "Route exists but probe data/method doesn't match production use"
            }
            Self::Performance => "Slow or timed out — may be env load, not a broken API",
        }
    }

    pub fn all() -> [Self; 4] {
        [
            Self::Severe,
            Self::Auth,
            Self::Compatibility,
            Self::Performance,
        ]
    }
}

#[derive(Debug, Clone)]
pub struct ClassifiedIssue {
    pub category: IssueCategory,
    pub feature_label: String,
    pub step_name: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub detail: String,
    pub hint: String,
}

#[derive(Debug, Clone)]
pub struct CategorizedRunReport {
    pub base_url: String,
    pub total_features: usize,
    pub clean_features: usize,
    pub issues: BTreeMap<IssueCategory, Vec<ClassifiedIssue>>,
    pub issue_count: usize,
}

pub fn build_categorized_report(report: &FeatureRunReport) -> CategorizedRunReport {
    let mut issues: BTreeMap<IssueCategory, Vec<ClassifiedIssue>> = BTreeMap::new();
    let mut features_with_issues = 0usize;

    for result in &report.results {
        let feature_issues = collect_feature_issues(result);
        if feature_issues.is_empty() {
            continue;
        }
        features_with_issues += 1;
        for issue in feature_issues {
            issues.entry(issue.category).or_default().push(issue);
        }
    }

    let issue_count = issues.values().map(|v| v.len()).sum();
    CategorizedRunReport {
        base_url: report.base_url.clone(),
        total_features: report.results.len(),
        clean_features: report.results.len().saturating_sub(features_with_issues),
        issues,
        issue_count,
    }
}

fn collect_feature_issues(result: &FeatureResult) -> Vec<ClassifiedIssue> {
    if result.steps.is_empty() {
        return classify_flat_result(result).into_iter().collect();
    }
    result
        .steps
        .iter()
        .filter_map(|step| {
            classify_step(&result.feature.label, step, is_write_flow(&result.feature))
        })
        .collect()
}

fn classify_flat_result(result: &FeatureResult) -> Option<ClassifiedIssue> {
    if result.verdict == ProbeVerdict::Healthy
        && !result.message.to_ascii_lowercase().contains("warn")
    {
        return None;
    }
    let write = is_write_flow(&result.feature);
    let pseudo = StepOutcome {
        name: result.feature.label.clone(),
        method: result
            .feature
            .method
            .clone()
            .unwrap_or_else(|| "GET".into()),
        path: result.feature.url.clone(),
        status: result.status,
        verdict: result.verdict,
        message: result.message.clone(),
        captured_token: false,
    };
    classify_step(&result.feature.label, &pseudo, write)
}

pub fn classify_step(
    feature_label: &str,
    step: &StepOutcome,
    write_flow: bool,
) -> Option<ClassifiedIssue> {
    if step.verdict == ProbeVerdict::Healthy {
        return None;
    }

    let (category, hint) = categorize(step, write_flow);
    let step_name = if step.name == feature_label {
        None
    } else {
        Some(step.name.clone())
    };

    Some(ClassifiedIssue {
        category,
        feature_label: feature_label.to_string(),
        step_name,
        method: Some(step.method.clone()),
        path: Some(step.path.clone()),
        detail: step.message.clone(),
        hint: hint.to_string(),
    })
}

fn categorize(step: &StepOutcome, write_flow: bool) -> (IssueCategory, &'static str) {
    let msg = step.message.to_ascii_lowercase();
    let path = step.path.to_ascii_lowercase();
    let status = step.status.unwrap_or(0);

    if is_transient_error_message(&step.message) || msg.contains("slow response") {
        return (
            IssueCategory::Performance,
            "Try --timeout 60s or rerun off-peak; route may be cold/slow",
        );
    }

    if msg.contains("auth skipped")
        || msg.contains("captcha")
        || msg.contains("missing access_token")
        || msg.contains("contract: missing")
    {
        return (
            IssueCategory::Auth,
            "Set bearer token / realm creds in auth popup (c) or env vars",
        );
    }

    if status == 401 || status == 403 || msg.contains("auth required") {
        return (
            IssueCategory::Auth,
            "Provide bearer token or login credentials for this realm",
        );
    }

    if is_placeholder_path(&path) || status == 404 {
        return (
            IssueCategory::Compatibility,
            "Probe uses placeholder path IDs — real resource IDs needed for HTTP 200",
        );
    }

    if write_flow && matches!(status, 400 | 422 | 502) {
        return (
            IssueCategory::Compatibility,
            "Write smoke sends synthetic payload; failure may not indicate a production bug",
        );
    }

    if matches!(status, 400 | 405 | 422)
        || (msg.contains("expected") && step.verdict != ProbeVerdict::Failed)
    {
        return (
            IssueCategory::Compatibility,
            "Route reachable but probe body/query/params don't match the API contract",
        );
    }

    if status >= 500 {
        return (
            IssueCategory::Severe,
            "Server error — check backend logs, dependencies, and deployment health",
        );
    }

    if step.verdict == ProbeVerdict::Failed {
        return (
            IssueCategory::Severe,
            "Probe failed — verify service health, URL, and auth configuration",
        );
    }

    (
        IssueCategory::Compatibility,
        "Route responded but not with success semantics for this probe",
    )
}

fn is_placeholder_path(path: &str) -> bool {
    path.contains('{')
        || path.contains("00000000-0000-0000")
        || path.contains("mock-")
        || path.contains("placeholder")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{ProbeVerdict, StepOutcome};

    fn step(status: u16, verdict: ProbeVerdict, message: &str, path: &str) -> StepOutcome {
        StepOutcome {
            name: "probe".into(),
            method: "GET".into(),
            path: path.into(),
            status: Some(status),
            verdict,
            message: message.into(),
            captured_token: false,
        }
    }

    #[test]
    fn timeout_is_performance() {
        let s = step(
            0,
            ProbeVerdict::Reachable,
            "timeout / slow response (deadline)",
            "/api/admin/queue",
        );
        let issue = classify_step("admin flow", &s, false).unwrap();
        assert_eq!(issue.category, IssueCategory::Performance);
    }

    #[test]
    fn placeholder_404_is_compatibility() {
        let s = step(
            404,
            ProbeVerdict::Reachable,
            "HTTP 404 (route reachable)",
            "/api/problems/00000000-0000-0000-0000-000000000000",
        );
        let issue = classify_step("problems", &s, false).unwrap();
        assert_eq!(issue.category, IssueCategory::Compatibility);
    }

    #[test]
    fn write_502_is_compatibility() {
        let s = step(502, ProbeVerdict::Failed, "HTTP 502", "/api/billing/cancel");
        let issue = classify_step("billing write smoke", &s, true).unwrap();
        assert_eq!(issue.category, IssueCategory::Compatibility);
    }

    #[test]
    fn server_500_is_severe() {
        let s = step(500, ProbeVerdict::Failed, "HTTP 500", "/api/health");
        let issue = classify_step("health", &s, false).unwrap();
        assert_eq!(issue.category, IssueCategory::Severe);
    }

    #[test]
    fn captcha_is_auth() {
        let s = step(
            400,
            ProbeVerdict::Reachable,
            "HTTP 400 (login blocked by captcha — set captcha_token or use bearer)",
            "/api/auth/login",
        );
        let issue = classify_step("user flow", &s, false).unwrap();
        assert_eq!(issue.category, IssueCategory::Auth);
    }
}
