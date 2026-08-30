//! Multi-step workflow scenarios (LLM-generated or heuristic from OpenAPI).

use crate::error::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::Instant;
use url::Url;

use super::auth::{AuthMode, AuthProfiles};
use super::openapi_index::{
    self, infer_realm, infer_realm_from_tag, AuthRealmHint, EndpointOp, OpenApiIndex,
};
use super::{
    aggregate_workflow_verdict, classify_status, is_transient_probe_error, FeatureResult,
    ProbeSettings, ProbeVerdict, StepOutcome,
};

pub const MANIFEST_VERSION: u32 = 5;

/// Max endpoint steps per flow (excluding login).
const ENDPOINTS_PER_FLOW: usize = 5;
/// Cap total workflow rows in the TUI.
const MAX_WORKFLOWS: usize = 48;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    #[default]
    Read,
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowManifest {
    #[serde(default)]
    pub manifest_version: u32,
    pub base_url: String,
    pub workflows: Vec<WorkflowScenario>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowScenario {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: FlowKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_realm: Option<String>,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_realm: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_auth_mode")]
    pub auth_mode: AuthMode,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub query: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_path: Option<String>,
    #[serde(default)]
    pub use_bearer: bool,
    /// JSON field to capture as bearer token after this step (e.g. access_token).
    #[serde(default)]
    pub capture_bearer: Option<String>,
    /// If set, this exact status is required (2xx mismatch on login stays reachable when unauthenticated).
    #[serde(default)]
    pub expect_status: Option<u16>,
}

fn is_default_auth_mode(m: &AuthMode) -> bool {
    *m == AuthMode::None
}

struct FlowContext<'a> {
    realm: AuthRealmHint,
    has_token_capture: bool,
    kind: FlowKind,
    index: &'a OpenApiIndex,
}

pub fn workflow_to_feature(base: &str, w: &WorkflowScenario) -> crate::features::DetectedFeature {
    let base_url = normalize_base(base);
    crate::features::DetectedFeature {
        id: w.id.clone(),
        label: w.label.clone(),
        url: base_url,
        kind: crate::features::FeatureKind::Workflow,
        source: if matches!(w.kind, FlowKind::Write) {
            "workflow-write".into()
        } else {
            "workflow".into()
        },
        method: None,
        workflow: Some(w.clone()),
    }
}

pub async fn run_workflow(
    base: &str,
    scenario: &WorkflowScenario,
    settings: &ProbeSettings,
) -> FeatureResult {
    let feature = workflow_to_feature(base, scenario);
    let start = Instant::now();
    let client = match Client::builder().timeout(settings.timeout).build() {
        Ok(c) => c,
        Err(e) => {
            return fail_feature(feature, format!("HTTP client: {e}"), start);
        }
    };

    let root = match Url::parse(&normalize_base(base)) {
        Ok(u) => u,
        Err(e) => return fail_feature(feature, format!("invalid base URL: {e}"), start),
    };

    let auth: &AuthProfiles = &settings.auth;
    let flow_realm = scenario
        .auth_realm
        .as_deref()
        .and_then(|s| s.parse::<AuthRealmHint>().ok())
        .unwrap_or(AuthRealmHint::User);
    let mut token = auth.bearer_for_realm(flow_realm).map(|s| s.to_string());
    if token.is_none() {
        token = auth.bearer().map(|s| s.to_string());
    }
    let mut last_status: Option<u16> = None;
    let mut first_ms: Option<f64> = None;
    let mut step_msgs = Vec::new();
    let mut outcomes: Vec<StepOutcome> = Vec::new();

    for step in &scenario.steps {
        let step_realm = step
            .auth_realm
            .as_deref()
            .and_then(|s| s.parse::<AuthRealmHint>().ok())
            .unwrap_or(flow_realm);

        if !auth.realm_ready(step_realm) && step_realm != AuthRealmHint::Public {
            step_msgs.push(format!(
                "{}: skipped (no {} creds)",
                step.name,
                step_realm.as_str()
            ));
            outcomes.push(StepOutcome {
                name: step.name.clone(),
                method: step.method.clone(),
                path: step.path.clone(),
                status: None,
                verdict: ProbeVerdict::Reachable,
                message: format!("auth skipped (no {} creds)", step_realm.as_str()),
                captured_token: false,
            });
            continue;
        }

        if step.capture_bearer.is_some() && auth.skip_login_capture(step_realm) {
            step_msgs.push(format!("{}: skipped (bearer profile)", step.name));
            continue;
        }

        let probe_path = step.probe_path.as_deref().unwrap_or(&step.path);
        let mut url = join_path(&root, probe_path);
        if let Some(qs) = step.query.as_ref().and_then(query_from_value) {
            if !qs.is_empty() {
                url = format!("{url}?{qs}");
            }
        }

        let method = step.method.to_ascii_uppercase();
        let mut req = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
            &url,
        );

        if step.use_bearer {
            if let Some(t) = &token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
        }

        for (name, value) in auth.auth_headers(step_realm, step.auth_mode.clone(), None) {
            req = req.header(name, value);
        }

        if let Some(body) = &step.body {
            req = req.json(&auth.resolve_body(body, step_realm));
        }

        let step_start = Instant::now();
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                last_status = Some(status);
                if first_ms.is_none() {
                    first_ms = Some(step_start.elapsed().as_secs_f64() * 1000.0);
                }
                let body_text = resp.text().await.unwrap_or_default();

                let mut captured = false;
                if let Some(field) = &step.capture_bearer {
                    if let Ok(json) = serde_json::from_str::<Value>(&body_text) {
                        if let Some(t) = json.get(field).and_then(|v| v.as_str()) {
                            if !t.is_empty() {
                                token = Some(t.to_string());
                                captured = true;
                            }
                        }
                    }
                }

                let (mut verdict, mut message) = apply_step_contract(step, status, captured);
                if step.capture_bearer.is_some()
                    && status == 400
                    && body_text.to_ascii_lowercase().contains("captcha")
                {
                    verdict = ProbeVerdict::Reachable;
                    message =
                        "HTTP 400 (login blocked by captcha — set captcha_token or use bearer)"
                            .into();
                }
                step_msgs.push(format!("{}: {message}", step.name));
                outcomes.push(StepOutcome {
                    name: step.name.clone(),
                    method: method.clone(),
                    path: step.path.clone(),
                    status: Some(status),
                    verdict,
                    message,
                    captured_token: captured,
                });
            }
            Err(e) => {
                let transient = is_transient_probe_error(&e);
                let verdict = if transient {
                    ProbeVerdict::Reachable
                } else {
                    ProbeVerdict::Failed
                };
                let message = if transient {
                    format!("timeout / slow response ({e})")
                } else {
                    e.to_string()
                };
                step_msgs.push(format!("{}: {message}", step.name));
                outcomes.push(StepOutcome {
                    name: step.name.clone(),
                    method: method.clone(),
                    path: step.path.clone(),
                    status: None,
                    verdict,
                    message,
                    captured_token: false,
                });
            }
        }
    }

    let verdict = aggregate_workflow_verdict(&outcomes);
    let ok = verdict != ProbeVerdict::Failed;
    FeatureResult {
        feature,
        ok,
        verdict,
        status: last_status,
        total_ms: start.elapsed().as_secs_f64() * 1000.0,
        ttfb_ms: first_ms,
        message: format!(
            "{} — {} steps ({})",
            scenario.label,
            outcomes.len(),
            step_msgs.join("; ")
        ),
        l7: None,
        steps: outcomes,
    }
}

fn apply_step_contract(step: &WorkflowStep, status: u16, captured: bool) -> (ProbeVerdict, String) {
    let mut verdict = classify_status(status);
    let mut message = super::status_message(status, verdict);

    if let Some(expected) = step.expect_status {
        if status != expected {
            verdict = if (400..=422).contains(&status) {
                ProbeVerdict::Reachable
            } else {
                ProbeVerdict::Failed
            };
            message = format!("HTTP {status} (expected {expected})");
        }
    }

    if let Some(field) = &step.capture_bearer {
        if (200..400).contains(&status) && !captured {
            verdict = ProbeVerdict::Failed;
            message = format!("HTTP {status} (contract: missing {field})");
        }
    }

    (verdict, message)
}

fn fail_feature(
    feature: crate::features::DetectedFeature,
    message: String,
    start: Instant,
) -> FeatureResult {
    fail_feature_with_steps(feature, message, start, Vec::new())
}

fn fail_feature_with_steps(
    feature: crate::features::DetectedFeature,
    message: String,
    start: Instant,
    steps: Vec<StepOutcome>,
) -> FeatureResult {
    FeatureResult {
        feature,
        ok: false,
        verdict: ProbeVerdict::Failed,
        status: None,
        total_ms: start.elapsed().as_secs_f64() * 1000.0,
        ttfb_ms: None,
        message,
        l7: None,
        steps,
    }
}

fn normalize_base(base: &str) -> String {
    let t = base.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

fn join_path(root: &Url, path: &str) -> String {
    let mut u = root.clone();
    u.set_path(path);
    u.to_string()
}

pub fn load_workflow_manifest(
    path: &Path,
    base: &str,
) -> Result<Vec<crate::features::DetectedFeature>> {
    let text = std::fs::read_to_string(path)?;
    let manifest: WorkflowManifest = serde_json::from_str(&text)?;
    if manifest.manifest_version != MANIFEST_VERSION && manifest.manifest_version != 4 {
        return Ok(Vec::new());
    }
    Ok(manifest
        .workflows
        .iter()
        .map(|w| workflow_to_feature(base, w))
        .collect())
}

/// Build validated workflow candidates from OpenAPI JSON.
pub fn heuristic_workflows_from_openapi(openapi_json: &str) -> Vec<WorkflowScenario> {
    let index = OpenApiIndex::from_json(openapi_json);
    let workflows = heuristic_workflows_from_index(&index);
    validate_and_filter_workflows(workflows, &index)
}

pub fn heuristic_workflows_from_index(index: &OpenApiIndex) -> Vec<WorkflowScenario> {
    let mut workflows = Vec::new();

    if let Some(health) = build_health_smoke(index) {
        workflows.push(health);
    }
    if let Some(auth) = build_auth_smoke(index) {
        workflows.push(auth);
    }
    workflows.extend(build_domain_flows(index));
    workflows.extend(build_uncovered_flows(index, &workflows));
    workflows.truncate(40);
    workflows.extend(build_write_smokes(index));
    workflows.truncate(MAX_WORKFLOWS);
    workflows
}

pub fn validate_and_filter_workflows(
    workflows: Vec<WorkflowScenario>,
    index: &OpenApiIndex,
) -> Vec<WorkflowScenario> {
    workflows
        .into_iter()
        .filter(|w| match validate_workflow(w, index) {
            Ok(()) => true,
            Err(reasons) => {
                tracing::debug!(workflow = %w.id, ?reasons, "dropped invalid workflow");
                false
            }
        })
        .collect()
}

pub fn validate_workflow(
    w: &WorkflowScenario,
    index: &OpenApiIndex,
) -> std::result::Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if w.steps.is_empty() {
        errors.push("workflow has no steps".into());
    }

    if w.steps.len() == 1 && w.id != "health_smoke" {
        errors.push("single-step workflow only allowed for health_smoke".into());
    }

    let mut seen_paths = std::collections::HashSet::new();
    let mut has_token_capture = false;

    for (i, step) in w.steps.iter().enumerate() {
        let key = format!("{}:{}", step.method.to_ascii_uppercase(), step.path);
        if !seen_paths.insert(key) {
            errors.push(format!("duplicate step: {} {}", step.method, step.path));
        }

        if w.id == "health_smoke" && !is_strict_health_step(step, index) {
            errors.push(format!(
                "health_smoke contains non-health step: {} {}",
                step.method, step.path
            ));
        }

        if step.use_bearer && !has_token_capture {
            let static_auth = matches!(
                step.auth_mode,
                AuthMode::BearerStatic | AuthMode::ApiKeyHeader { .. }
            );
            if !static_auth {
                errors.push(format!(
                    "step {} uses bearer without prior token capture",
                    i + 1
                ));
            }
        }

        if step.capture_bearer.is_some() {
            has_token_capture = true;
        }

        if step.path.contains('{') {
            // allowed but counted below
        }

        if let Some(op) = index.find(&step.path, &step.method) {
            if step.operation_id.is_none() && !op.operation_id.is_empty() {
                // informational only at build time
            }
        } else if w.id != "health_smoke" && !step.path.starts_with("/health") {
            errors.push(format!(
                "step not in OpenAPI index: {} {}",
                step.method, step.path
            ));
        }
    }

    let param_steps = w.steps.iter().filter(|s| s.path.contains('{')).count();
    if w.steps.len() > 1 && param_steps * 2 > w.steps.len() {
        errors.push(format!(
            "too many path-param steps ({param_steps}/{}",
            w.steps.len()
        ));
    }

    if (w.id.ends_with("_flow") || w.id.contains("_flow_")) && w.kind != FlowKind::Write {
        let non_login: Vec<_> = w
            .steps
            .iter()
            .filter(|s| s.capture_bearer.is_none())
            .collect();
        if let Some(first) = non_login.first() {
            if matches!(
                first.method.to_ascii_uppercase().as_str(),
                "POST" | "PUT" | "PATCH" | "DELETE"
            ) {
                errors.push("domain flow starts with write/destructive step".into());
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn build_health_smoke(index: &OpenApiIndex) -> Option<WorkflowScenario> {
    let mut candidates: Vec<&EndpointOp> = index
        .ops
        .iter()
        .filter(|op| {
            op.is_get() && !op.requires_auth && !op.has_path_params() && is_strict_health_op(op)
        })
        .collect();

    candidates.sort_by_key(|b| std::cmp::Reverse(health_rank(b)));
    candidates.dedup_by(|a, b| a.path == b.path);

    let steps: Vec<WorkflowStep> = candidates
        .into_iter()
        .take(3)
        .map(|op| {
            let mut ctx = FlowContext {
                realm: AuthRealmHint::Public,
                has_token_capture: false,
                kind: FlowKind::Read,
                index,
            };
            materialize_step(op, &mut ctx)
        })
        .collect();

    if steps.is_empty() {
        return None;
    }

    Some(WorkflowScenario {
        id: "health_smoke".into(),
        label: "Health smoke".into(),
        description: "Public health and readiness".into(),
        kind: FlowKind::Read,
        auth_realm: None,
        steps,
    })
}

fn build_auth_smoke(index: &OpenApiIndex) -> Option<WorkflowScenario> {
    let realm = AuthRealmHint::User;
    let login = index.login_for_realm(realm)?;
    let mut ctx = FlowContext {
        realm,
        has_token_capture: false,
        kind: FlowKind::Read,
        index,
    };
    let mut steps = vec![login_step_for_realm(login, realm, &mut ctx)];

    let mut profiles: Vec<&EndpointOp> = index
        .ops
        .iter()
        .filter(|op| {
            op.is_get()
                && (op.requires_auth || infer_realm(op) == AuthRealmHint::User)
                && is_profile_path(&op.path)
                && !op.path.to_lowercase().contains("login")
        })
        .collect();
    profiles.sort_by_key(|op| profile_rank(op));

    for op in profiles.into_iter().take(5) {
        steps.push(materialize_step(op, &mut ctx));
    }

    if steps.len() < 2 {
        return None;
    }

    Some(WorkflowScenario {
        id: "auth_smoke".into(),
        label: "Auth smoke".into(),
        description: "Login and authenticated reads (set TRACE_DIFF_EMAIL / TRACE_DIFF_PASSWORD)"
            .into(),
        kind: FlowKind::Read,
        auth_realm: Some(realm.as_str().into()),
        steps,
    })
}

fn build_domain_flows(index: &OpenApiIndex) -> Vec<WorkflowScenario> {
    let mut workflows = Vec::new();

    for tag in index.all_tags() {
        if tag.eq_ignore_ascii_case("health") || tag.eq_ignore_ascii_case("auth") {
            continue;
        }

        let realm = infer_realm_from_tag(&tag);
        let ops = eligible_domain_ops(index, &tag);
        workflows.extend(chunk_ops_into_flows(&tag, ops, index, realm, 0));
    }

    workflows
}

/// Second pass: bucket remaining GET endpoints not yet assigned to any flow.
fn build_uncovered_flows(
    index: &OpenApiIndex,
    existing: &[WorkflowScenario],
) -> Vec<WorkflowScenario> {
    let covered = covered_step_keys(existing);

    let mut by_tag: std::collections::BTreeMap<String, Vec<&EndpointOp>> =
        std::collections::BTreeMap::new();

    for op in &index.ops {
        if !op.is_get() || is_strict_health_op(op) || op.path.to_lowercase().contains("login") {
            continue;
        }
        let key = step_key(&op.method, &op.path);
        if covered.contains(&key) {
            continue;
        }
        if score_domain_op(op) < 0 {
            continue;
        }
        by_tag.entry(op.primary_tag()).or_default().push(op);
    }

    let mut workflows = Vec::new();
    for (tag, mut ops) in by_tag {
        if tag.eq_ignore_ascii_case("health") || tag.eq_ignore_ascii_case("auth") {
            continue;
        }
        ops.sort_by(|a, b| {
            score_domain_op(b)
                .cmp(&score_domain_op(a))
                .then_with(|| a.path.cmp(&b.path))
        });
        ops.dedup_by(|a, b| a.path == b.path && a.method == b.method);
        let realm = infer_realm_from_tag(&tag);
        workflows.extend(chunk_ops_into_flows(&tag, ops, index, realm, 1));
    }

    workflows
}

fn eligible_domain_ops<'a>(index: &'a OpenApiIndex, tag: &str) -> Vec<&'a EndpointOp> {
    let mut ops: Vec<&EndpointOp> = index
        .by_tag(tag)
        .into_iter()
        .filter(|op| {
            op.is_get()
                && !is_strict_health_op(op)
                && !op.path.to_lowercase().contains("login")
                && score_domain_op(op) >= 0
        })
        .collect();
    ops.sort_by(|a, b| {
        score_domain_op(b)
            .cmp(&score_domain_op(a))
            .then_with(|| a.path.cmp(&b.path))
    });
    ops.dedup_by(|a, b| a.path == b.path && a.method == b.method);
    ops
}

fn chunk_ops_into_flows(
    tag: &str,
    ops: Vec<&EndpointOp>,
    index: &OpenApiIndex,
    realm: AuthRealmHint,
    id_offset: usize,
) -> Vec<WorkflowScenario> {
    if ops.is_empty() {
        return Vec::new();
    }

    let needs_auth = realm != AuthRealmHint::Public
        && (ops.iter().any(|op| op_needs_auth(op)) || realm.uses_login_capture());
    let mut chunks: Vec<Vec<&EndpointOp>> =
        ops.chunks(ENDPOINTS_PER_FLOW).map(|c| c.to_vec()).collect();

    // Avoid a orphan single-endpoint public chunk (validation requires 2+ steps).
    if !needs_auth && chunks.len() > 1 {
        if let Some(orphan) = chunks.pop_if(|last| last.len() == 1) {
            if let Some(prev) = chunks.last_mut() {
                prev.extend(orphan);
            }
        }
    }

    let slug = tag.to_lowercase().replace([' ', '-'], "_");
    let mut flows = Vec::new();

    for (chunk_i, chunk) in chunks.into_iter().enumerate() {
        let mut ctx = FlowContext {
            realm,
            has_token_capture: false,
            kind: FlowKind::Read,
            index,
        };
        let mut steps = Vec::new();

        if needs_auth && realm.uses_login_capture() {
            if let Some(login_op) = index.login_for_realm(realm) {
                steps.push(login_step_for_realm(login_op, realm, &mut ctx));
            } else {
                continue;
            }
        }

        for op in &chunk {
            steps.push(materialize_step(op, &mut ctx));
        }

        if needs_auth && realm.uses_login_capture() && steps.len() <= 1 {
            continue;
        }
        if !needs_auth && steps.len() < 2 {
            continue;
        }

        let id = if chunk_i == 0 && id_offset == 0 {
            format!("{slug}_flow")
        } else if id_offset > 0 {
            format!("{slug}_extra_{}", chunk_i + 1)
        } else {
            format!("{slug}_flow_{}", chunk_i + 1)
        };
        let label = if chunk_i == 0 && id_offset == 0 {
            format!("{tag} flow")
        } else if id_offset > 0 {
            format!("{tag} extra {}", chunk_i + 1)
        } else {
            format!("{tag} flow {}", chunk_i + 1)
        };

        flows.push(WorkflowScenario {
            id,
            label,
            description: format!("{tag} endpoints ({} steps)", steps.len()),
            kind: FlowKind::Read,
            auth_realm: if realm == AuthRealmHint::Public {
                None
            } else {
                Some(realm.as_str().into())
            },
            steps,
        });
    }

    flows
}

fn build_write_smokes(index: &OpenApiIndex) -> Vec<WorkflowScenario> {
    let mut workflows = Vec::new();
    for tag in index.all_tags() {
        if tag.eq_ignore_ascii_case("health") || tag.eq_ignore_ascii_case("auth") {
            continue;
        }
        let realm = infer_realm_from_tag(&tag);
        let mut ops: Vec<&EndpointOp> = index
            .by_tag(&tag)
            .into_iter()
            .filter(|op| {
                op.is_write_method()
                    && !op.path.to_lowercase().contains("login")
                    && !op.has_path_params()
                    && score_write_op(op) >= 0
            })
            .collect();
        ops.sort_by(|a, b| {
            score_write_op(b)
                .cmp(&score_write_op(a))
                .then_with(|| a.path.cmp(&b.path))
        });
        ops.dedup_by(|a, b| a.path == b.path && a.method == b.method);
        let top: Vec<&EndpointOp> = ops.into_iter().take(3).collect();
        if top.is_empty() {
            continue;
        }
        let mut ctx = FlowContext {
            realm,
            has_token_capture: false,
            kind: FlowKind::Write,
            index,
        };
        let mut steps = Vec::new();
        if realm.uses_login_capture() {
            if let Some(login_op) = index.login_for_realm(realm) {
                steps.push(login_step_for_realm(login_op, realm, &mut ctx));
            }
        }
        for op in top {
            steps.push(materialize_step(op, &mut ctx));
        }
        if steps.len() < 2 {
            continue;
        }
        let slug = tag.to_lowercase().replace([' ', '-'], "_");
        workflows.push(WorkflowScenario {
            id: format!("{slug}_write"),
            label: format!("{tag} write smoke"),
            description: format!("{tag} mutating endpoints (not selected by default)"),
            kind: FlowKind::Write,
            auth_realm: if realm == AuthRealmHint::Public {
                None
            } else {
                Some(realm.as_str().into())
            },
            steps,
        });
        if workflows.len() >= 8 {
            break;
        }
    }
    workflows
}

fn score_write_op(op: &EndpointOp) -> i32 {
    let mut score = 10;
    if op.method == "POST" {
        score += 5;
    }
    if op.has_path_params() {
        score -= 20;
    }
    let hay = format!("{} {}", op.path.to_lowercase(), op.summary.to_lowercase());
    if hay.contains("delete") || hay.contains("destroy") {
        score -= 40;
    }
    if hay.contains("checkout") || hay.contains("refund") {
        score -= 25;
    }
    score
}

fn covered_step_keys(workflows: &[WorkflowScenario]) -> std::collections::HashSet<String> {
    let mut covered = std::collections::HashSet::new();
    for w in workflows {
        for step in &w.steps {
            if step.capture_bearer.is_some() {
                continue;
            }
            covered.insert(step_key(&step.method, &step.path));
        }
    }
    covered
}

fn step_key(method: &str, path: &str) -> String {
    format!("{}:{}", method.to_ascii_uppercase(), path)
}

fn op_needs_auth(op: &EndpointOp) -> bool {
    op.requires_auth || looks_protected_path(&op.path)
}

fn looks_protected_path(path: &str) -> bool {
    path.contains("/admin/")
        || path.contains("/auth/")
        || path.contains("/user")
        || path.contains("/account")
        || path.contains("/billing")
}

fn login_step_for_realm(
    op: &EndpointOp,
    realm: AuthRealmHint,
    ctx: &mut FlowContext<'_>,
) -> WorkflowStep {
    ctx.has_token_capture = true;
    let body = login_body_for_realm(op, realm);
    let capture = op
        .login_token_field()
        .unwrap_or_else(|| "access_token".into());
    let expect = op.default_expect_status().or(Some(200));
    WorkflowStep {
        name: "login".into(),
        method: op.method.clone(),
        path: op.path.clone(),
        operation_id: if op.operation_id.is_empty() {
            None
        } else {
            Some(op.operation_id.clone())
        },
        auth_realm: Some(realm.as_str().into()),
        auth_mode: AuthMode::BearerCapture,
        body: Some(body),
        query: query_value(op),
        probe_path: Some(op.probe_path()),
        capture_bearer: Some(capture),
        expect_status: expect,
        ..Default::default()
    }
}

fn login_body_for_realm(op: &EndpointOp, realm: AuthRealmHint) -> Value {
    if let Some(body_spec) = &op.request_body {
        if body_spec.is_credential_login() || !body_spec.property_names.is_empty() {
            let mut body = serde_json::Map::new();
            for prop in &body_spec.property_names {
                let ph = login_placeholder_for_field(prop, realm);
                body.insert(prop.clone(), Value::String(ph));
            }
            if body.is_empty() {
                body.insert(
                    "email".into(),
                    Value::String(login_placeholder_for_field("email", realm)),
                );
                body.insert(
                    "password".into(),
                    Value::String(login_placeholder_for_field("password", realm)),
                );
            }
            return Value::Object(body);
        }
        return body_spec.minimal_json();
    }
    match realm {
        AuthRealmHint::Annotator => serde_json::json!({
            "email": "${TRACE_DIFF_ANNOTATOR_EMAIL}",
            "password": "${TRACE_DIFF_ANNOTATOR_PASSWORD}"
        }),
        _ => serde_json::json!({
            "email": "${CONFUCIUS_EMAIL}",
            "password": "${CONFUCIUS_PASSWORD}"
        }),
    }
}

fn login_placeholder_for_field(name: &str, realm: AuthRealmHint) -> String {
    match name.to_ascii_lowercase().as_str() {
        "email" => match realm {
            AuthRealmHint::Annotator => "${TRACE_DIFF_ANNOTATOR_EMAIL}".into(),
            _ => "${CONFUCIUS_EMAIL}".into(),
        },
        "password" => match realm {
            AuthRealmHint::Annotator => "${TRACE_DIFF_ANNOTATOR_PASSWORD}".into(),
            _ => "${CONFUCIUS_PASSWORD}".into(),
        },
        "captcha_token" | "captcha" => "${TRACE_DIFF_CAPTCHA_TOKEN}".into(),
        other => format!("${{{}}}", other.to_ascii_uppercase()),
    }
}

fn materialize_step(op: &EndpointOp, ctx: &mut FlowContext<'_>) -> WorkflowStep {
    let index = ctx.index;
    let realm = ctx.realm;
    let use_bearer = needs_bearer(op, ctx);
    let auth_mode = auth_mode_for_op(op, realm, index, use_bearer);

    let body = if ctx.kind == FlowKind::Write && op.has_request_body {
        op.request_body
            .as_ref()
            .map(|b| b.minimal_json())
            .or_else(|| Some(serde_json::json!({})))
    } else {
        None
    };

    let expect_status = op.default_expect_status();

    WorkflowStep {
        name: path_to_step_name(&op.path),
        method: op.method.clone(),
        path: op.path.clone(),
        operation_id: if op.operation_id.is_empty() {
            None
        } else {
            Some(op.operation_id.clone())
        },
        auth_realm: if realm == AuthRealmHint::Public {
            None
        } else {
            Some(realm.as_str().into())
        },
        auth_mode,
        body,
        query: query_value(op),
        probe_path: Some(op.probe_path()),
        use_bearer,
        expect_status,
        ..Default::default()
    }
}

fn needs_bearer(op: &EndpointOp, ctx: &FlowContext<'_>) -> bool {
    if ctx.realm == AuthRealmHint::Admin {
        return false;
    }
    if ctx.has_token_capture && ctx.realm.uses_login_capture() {
        return true;
    }
    if op.has_bearer_scheme(ctx.index) {
        return true;
    }
    if ctx.realm.uses_login_capture() && !op.explicitly_public && op_needs_auth(op) {
        return true;
    }
    false
}

fn admin_secret_header_param(op: &EndpointOp) -> Option<&openapi_index::ParamSpec> {
    op.header_params.iter().find(|p| {
        let n = p.name.to_ascii_lowercase();
        n == "x-admin-secret" || n.contains("admin-secret") || n.contains("admin_secret")
    })
}

fn auth_mode_for_op(
    op: &EndpointOp,
    realm: AuthRealmHint,
    index: &OpenApiIndex,
    use_bearer: bool,
) -> AuthMode {
    if realm == AuthRealmHint::Admin {
        if let Some((_, header)) = op.primary_api_key_header(index) {
            return AuthMode::ApiKeyHeader {
                header_name: header,
            };
        }
        if let Some(h) = admin_secret_header_param(op) {
            return AuthMode::ApiKeyHeader {
                header_name: h.name.clone(),
            };
        }
        return AuthMode::ApiKeyHeader {
            header_name: "X-Admin-Secret".into(),
        };
    }
    if use_bearer {
        if realm.uses_login_capture() {
            AuthMode::BearerCapture
        } else {
            AuthMode::BearerStatic
        }
    } else {
        AuthMode::None
    }
}

fn query_value(op: &EndpointOp) -> Option<Value> {
    openapi_index::build_query_string(op).map(|qs| {
        let mut map = serde_json::Map::new();
        for pair in qs.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                map.insert(k.to_string(), Value::String(v.to_string()));
            }
        }
        Value::Object(map)
    })
}

fn query_from_value(q: &Value) -> Option<String> {
    let obj = q.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let pairs: Vec<String> = obj
        .iter()
        .map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or("probe")))
        .collect();
    Some(pairs.join("&"))
}

/// Collect distinct auth realms from generated workflows (for TUI popup).
pub fn detect_auth_realms(workflows: &[WorkflowScenario]) -> Vec<AuthRealmHint> {
    let mut set = std::collections::BTreeSet::new();
    for w in workflows {
        if let Some(r) = w
            .auth_realm
            .as_deref()
            .and_then(|s| s.parse::<AuthRealmHint>().ok())
        {
            if r != AuthRealmHint::Public {
                set.insert(r);
            }
        } else if w.id == "auth_smoke" {
            set.insert(AuthRealmHint::User);
        } else if w.id.contains("annotator") {
            set.insert(AuthRealmHint::Annotator);
        } else if w.id.contains("admin") {
            set.insert(AuthRealmHint::Admin);
        }
    }
    set.into_iter().collect()
}

pub fn workflow_realm(w: &WorkflowScenario) -> Option<AuthRealmHint> {
    w.auth_realm
        .as_deref()
        .and_then(|s| s.parse::<AuthRealmHint>().ok())
        .or_else(|| {
            if w.id.contains("admin") {
                Some(AuthRealmHint::Admin)
            } else if w.id.contains("annotator") {
                Some(AuthRealmHint::Annotator)
            } else if w.id == "auth_smoke" {
                Some(AuthRealmHint::User)
            } else {
                None
            }
        })
}

fn health_rank(op: &EndpointOp) -> i32 {
    let path = op.path.to_lowercase();
    if path == "/health" {
        return 100;
    }
    if path == "/api/health" {
        return 90;
    }
    if path.ends_with("/health") {
        return 70;
    }
    if path.ends_with("/ready") || path.ends_with("/live") || path.ends_with("/ping") {
        return 60;
    }
    0
}

fn profile_rank(op: &EndpointOp) -> i32 {
    let path = op.path.to_lowercase();
    if path.ends_with("/me") {
        return 100;
    }
    if path.contains("/auth/me") {
        return 90;
    }
    if path.contains("/profile") {
        return 80;
    }
    0
}

fn score_domain_op(op: &EndpointOp) -> i32 {
    let mut score = 0;
    if op.is_get() {
        score += 20;
    }
    if !op.has_path_params() {
        score += 10;
    }
    if op.is_write_method() {
        score -= 30;
    }
    let summary = op.summary.to_lowercase();
    if summary.contains("create")
        || summary.contains("delete")
        || summary.contains("checkout")
        || summary.contains("update")
    {
        score -= 25;
    }
    if op.path.to_lowercase().contains("checkout") {
        score -= 20;
    }
    score
}

fn is_strict_health_op(op: &EndpointOp) -> bool {
    is_strict_health_path(&op.path)
        || (op.primary_tag().eq_ignore_ascii_case("health")
            && op.summary.to_lowercase().contains("health"))
}

fn is_strict_health_path(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    let lower = p.to_lowercase();
    matches!(
        lower.as_str(),
        "/health"
            | "/api/health"
            | "/api/v1/health"
            | "/v1/health"
            | "/ready"
            | "/live"
            | "/ping"
            | "/api/ready"
            | "/api/live"
            | "/api/ping"
    )
}

fn is_strict_health_step(step: &WorkflowStep, index: &OpenApiIndex) -> bool {
    if !step.method.eq_ignore_ascii_case("GET") {
        return false;
    }
    if is_strict_health_path(&step.path) {
        return true;
    }
    index
        .find(&step.path, &step.method)
        .map(is_strict_health_op)
        .unwrap_or(false)
}

fn is_profile_path(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with("/me") || p.contains("/profile") || (p.contains("/auth/") && !p.contains("login"))
}

fn path_to_step_name(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .next_back()
        .unwrap_or("step")
        .replace(['{', '}'], "")
        .to_lowercase()
}

/// Fallback smoke workflows when OpenAPI is unavailable.
pub fn default_smoke_workflows(base: &str) -> Vec<crate::features::DetectedFeature> {
    let scenarios = [
        WorkflowScenario {
            id: "health_smoke".into(),
            label: "Health smoke".into(),
            description: "Public health endpoints".into(),
            kind: FlowKind::Read,
            auth_realm: None,
            steps: vec![
                WorkflowStep {
                    name: "health".into(),
                    method: "GET".into(),
                    path: "/health".into(),
                    ..Default::default()
                },
                WorkflowStep {
                    name: "api_health".into(),
                    method: "GET".into(),
                    path: "/api/health".into(),
                    ..Default::default()
                },
            ],
        },
        WorkflowScenario {
            id: "auth_smoke".into(),
            label: "Auth smoke".into(),
            description: "Login and profile (set TRACE_DIFF_EMAIL / TRACE_DIFF_PASSWORD)".into(),
            kind: FlowKind::Read,
            auth_realm: Some("user".into()),
            steps: vec![
                WorkflowStep {
                    name: "login".into(),
                    method: "POST".into(),
                    path: "/api/auth/login".into(),
                    auth_realm: Some("user".into()),
                    auth_mode: AuthMode::BearerCapture,
                    body: Some(serde_json::json!({
                        "email": "${CONFUCIUS_EMAIL}",
                        "password": "${CONFUCIUS_PASSWORD}"
                    })),
                    capture_bearer: Some("access_token".into()),
                    expect_status: Some(200),
                    ..Default::default()
                },
                WorkflowStep {
                    name: "me".into(),
                    method: "GET".into(),
                    path: "/api/auth/me".into(),
                    auth_realm: Some("user".into()),
                    auth_mode: AuthMode::BearerCapture,
                    use_bearer: true,
                    ..Default::default()
                },
            ],
        },
    ];
    scenarios
        .iter()
        .map(|w| workflow_to_feature(base, w))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_groups_auth_and_tags() {
        let spec = r#"{
            "security": [{"Bearer": []}],
            "paths": {
                "/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/auth/login": { "post": { "tags": ["auth"] } },
                "/api/annotators/login": { "post": { "tags": ["annotators"] } },
                "/api/auth/me": { "get": { "tags": ["auth"] } },
                "/api/admin/users": { "get": { "tags": ["admin"] } },
                "/api/admin/stats": { "get": { "tags": ["admin"] } }
            }
        }"#;
        let wfs = heuristic_workflows_from_openapi(spec);
        assert!(
            wfs.len() >= 3,
            "expected health + auth + admin, got {}",
            wfs.len()
        );
        assert!(wfs.iter().any(|w| w.id == "health_smoke"));
        assert!(wfs.iter().any(|w| w.id == "auth_smoke"));
        assert!(wfs.iter().any(|w| w.id == "admin_flow"));

        let health = wfs.iter().find(|w| w.id == "health_smoke").unwrap();
        assert!(
            !health.steps.iter().any(|s| s.path.contains("billing")),
            "health must not include billing/status"
        );

        let auth = wfs.iter().find(|w| w.id == "auth_smoke").unwrap();
        assert_eq!(auth.steps[0].path, "/api/auth/login");
    }

    #[test]
    fn health_excludes_billing_status() {
        let spec = r#"{
            "paths": {
                "/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/billing/status": { "get": { "tags": ["billing"], "security": [] } },
                "/api/saved-problems/{problem_id}/status": {
                    "get": { "tags": ["problems"], "security": [] }
                }
            }
        }"#;
        let wfs = heuristic_workflows_from_openapi(spec);
        let health = wfs.iter().find(|w| w.id == "health_smoke").unwrap();
        let paths: Vec<_> = health.steps.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["/health", "/api/health"]);
    }

    #[test]
    fn domain_tag_chunks_into_multiple_flows() {
        let mut paths = String::from(
            r#"{
            "security": [{"Bearer": []}],
            "paths": {
                "/api/auth/login": { "post": { "tags": ["auth"] } },
        "#,
        );
        for i in 0..12 {
            paths.push_str(&format!(
                r#""/api/admin/route{i}": {{ "get": {{ "tags": ["admin"], "summary": "Route {i}" }} }},"#
            ));
        }
        paths.push_str(r#""/api/health": { "get": { "tags": ["health"], "security": [] } } } }"#);

        let wfs = heuristic_workflows_from_openapi(&paths);
        let admin: Vec<_> = wfs.iter().filter(|w| w.id.contains("admin")).collect();
        assert!(
            admin.len() >= 2,
            "expected chunked admin flows, got {} admin workflows",
            admin.len()
        );
        let covered: usize = admin.iter().map(|w| w.steps.len()).sum();
        assert!(
            covered >= 10,
            "expected most admin routes covered, got {covered} steps"
        );
    }

    #[test]
    fn validation_rejects_bad_health_flow() {
        let index = OpenApiIndex::from_json("{}");
        let bad = WorkflowScenario {
            id: "health_smoke".into(),
            label: "bad".into(),
            description: String::new(),
            kind: FlowKind::Read,
            auth_realm: None,
            steps: vec![WorkflowStep {
                name: "billing".into(),
                method: "GET".into(),
                path: "/api/billing/status".into(),
                ..Default::default()
            }],
        };
        assert!(validate_workflow(&bad, &index).is_err());
    }

    #[test]
    fn login_200_without_token_fails_contract() {
        let step = WorkflowStep {
            name: "login".into(),
            capture_bearer: Some("access_token".into()),
            expect_status: Some(200),
            ..Default::default()
        };
        let (v, msg) = apply_step_contract(&step, 200, false);
        assert_eq!(v, ProbeVerdict::Failed);
        assert!(msg.contains("missing access_token"));
    }

    #[test]
    fn login_422_without_token_stays_reachable() {
        let step = WorkflowStep {
            name: "login".into(),
            capture_bearer: Some("access_token".into()),
            expect_status: Some(200),
            ..Default::default()
        };
        let (v, _) = apply_step_contract(&step, 422, false);
        assert_eq!(v, ProbeVerdict::Reachable);
    }

    #[test]
    fn annotator_flow_uses_annotator_login() {
        let spec = r#"{
            "paths": {
                "/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/auth/login": { "post": { "tags": ["auth"] } },
                "/api/annotators/login": { "post": { "tags": ["annotators"] } },
                "/api/annotators/me": { "get": { "tags": ["annotators"], "security": [] } }
            }
        }"#;
        let wfs = heuristic_workflows_from_openapi(spec);
        let ann = wfs
            .iter()
            .find(|w| w.id == "annotators_flow")
            .expect("annotators_flow");
        assert_eq!(ann.steps[0].path, "/api/annotators/login");
        assert!(ann.steps[1].use_bearer);
        assert_eq!(ann.auth_realm.as_deref(), Some("annotator"));
    }

    #[test]
    fn admin_flow_has_no_login_step() {
        let spec = r#"{
            "security": [{"AdminKey": []}],
            "components": {
                "securitySchemes": {
                    "AdminKey": { "type": "apiKey", "in": "header", "name": "X-Admin-Key" }
                }
            },
            "paths": {
                "/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/admin/users": { "get": { "tags": ["admin"] } },
                "/api/admin/stats": { "get": { "tags": ["admin"] } }
            }
        }"#;
        let wfs = heuristic_workflows_from_openapi(spec);
        let admin = wfs
            .iter()
            .find(|w| w.id == "admin_flow")
            .expect("admin_flow");
        assert!(!admin.steps.iter().any(|s| s.capture_bearer.is_some()));
        assert_eq!(admin.auth_realm.as_deref(), Some("admin"));
    }

    #[test]
    fn login_body_from_openapi_schema() {
        let spec = r##"{
            "components": {
                "schemas": {
                    "LoginRequest": {
                        "type": "object",
                        "required": ["email", "password"],
                        "properties": {
                            "email": { "type": "string" },
                            "password": { "type": "string" }
                        }
                    }
                }
            },
            "paths": {
                "/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/auth/login": {
                    "post": {
                        "tags": ["auth"],
                        "requestBody": {
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/LoginRequest" }
                                }
                            }
                        },
                        "responses": { "200": { "description": "ok" } }
                    }
                },
                "/api/auth/me": { "get": { "tags": ["auth"] } }
            }
        }"##;
        let wfs = heuristic_workflows_from_openapi(spec);
        let auth = wfs.iter().find(|w| w.id == "auth_smoke").unwrap();
        let body = auth.steps[0].body.as_ref().unwrap();
        assert!(body.get("email").is_some());
        assert!(body.get("password").is_some());
    }

    #[test]
    fn detect_auth_realms_from_workflows() {
        let wfs = vec![
            WorkflowScenario {
                id: "auth_smoke".into(),
                label: "Auth".into(),
                description: String::new(),
                kind: FlowKind::Read,
                auth_realm: Some("user".into()),
                steps: vec![],
            },
            WorkflowScenario {
                id: "admin_flow".into(),
                label: "Admin".into(),
                description: String::new(),
                kind: FlowKind::Read,
                auth_realm: Some("admin".into()),
                steps: vec![],
            },
        ];
        let realms = detect_auth_realms(&wfs);
        assert!(realms.contains(&AuthRealmHint::User));
        assert!(realms.contains(&AuthRealmHint::Admin));
    }

    #[test]
    fn write_smokes_are_tagged_separately() {
        let spec = r#"{
            "paths": {
                "/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/health": { "get": { "tags": ["health"], "security": [] } },
                "/api/auth/login": { "post": { "tags": ["auth"] } },
                "/api/admin/users": { "get": { "tags": ["admin"] } },
                "/api/admin/invite": { "post": { "tags": ["admin"] } },
                "/api/admin/settings": { "post": { "tags": ["admin"] } }
            }
        }"#;
        let wfs = heuristic_workflows_from_openapi(spec);
        let writes: Vec<_> = wfs.iter().filter(|w| w.kind == FlowKind::Write).collect();
        assert!(
            !writes.is_empty(),
            "expected a write-smoke FLOW, got {:?}",
            wfs.iter().map(|w| (&w.id, w.kind)).collect::<Vec<_>>()
        );
        assert!(writes.iter().all(|w| w.id.ends_with("_write")));
        let admin_reads: Vec<_> = wfs
            .iter()
            .filter(|w| w.id.contains("admin") && w.kind == FlowKind::Read)
            .collect();
        assert!(admin_reads.iter().all(|w| {
            !w.steps
                .iter()
                .any(|s| s.method.eq_ignore_ascii_case("POST") && s.path.contains("invite"))
        }));
    }
}
