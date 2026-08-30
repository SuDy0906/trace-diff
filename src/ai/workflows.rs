//! Infer and refine multi-step workflow scenarios from OpenAPI (background, no chat UI).

use super::provider::{complete_chat, AiConfig, ChatMessage};
use crate::error::{Error, Result};
use crate::features::{OpenApiIndex, WorkflowManifest, WorkflowScenario, MANIFEST_VERSION};
use serde_json::Value;
use std::path::Path;
use tracing::debug;

const REFINE_SYSTEM: &str = r#"You refine API test workflows for trace-diff (a network/API diagnostics CLI).

You receive a candidate workflow manifest and OpenAPI summary. Improve ordering, remove unrelated endpoints, and ensure auth chains are correct.

Output ONLY valid JSON (no markdown):
{
  "workflows": [
    {
      "id": "snake_case_id",
      "label": "Human readable name",
      "description": "What this scenario validates",
      "steps": [
        { "name": "step", "method": "GET", "path": "/api/health", "use_bearer": false },
        { "name": "login", "method": "POST", "path": "/api/auth/login",
          "body": { "email": "${CONFUCIUS_EMAIL}", "password": "${CONFUCIUS_PASSWORD}" },
          "capture_bearer": "access_token" },
        { "name": "me", "method": "GET", "path": "/api/auth/me", "use_bearer": true }
      ]
    }
  ]
}

Rules:
- Keep 4–8 workflows grouped by domain (health, auth, admin, billing, …).
- Do NOT invent paths — only use paths from the OpenAPI summary or candidate manifest.
- Health smoke: only public health/readiness GETs (/health, /api/health, /ready, /live, /ping).
- Never put /billing/status or resource /status endpoints in health smoke.
- Domain smokes: login → bearer GET reads; avoid checkout/create/delete POSTs in smokes.
- Use ${VAR} placeholders for secrets in login bodies.
- Each workflow: 2–5 steps max."#;

const GENERATE_SYSTEM: &str = r#"You infer API test workflows from OpenAPI specs for trace-diff (a network/API diagnostics CLI).

Output ONLY valid JSON (no markdown) with this shape:
{
  "workflows": [
    {
      "id": "snake_case_id",
      "label": "Human readable name",
      "description": "What this scenario validates",
      "steps": [
        {
          "name": "step_name",
          "method": "GET",
          "path": "/api/health",
          "use_bearer": false
        }
      ]
    }
  ]
}

Rules:
- Create 4–8 practical smoke/regression workflows grouped by domain (auth, admin, billing, public health).
- Prefer multi-step flows (login → authenticated GET) over listing every endpoint.
- Use env placeholders ${VAR} for secrets in POST bodies — never invent real credentials.
- Include at least one unauthenticated health/readiness workflow.
- Paths must come from the spec; do not invent routes.
- Each workflow: 2–5 steps max."#;

/// Refine validated heuristic candidates with LLM (hybrid pipeline).
pub async fn refine_workflows_from_openapi(
    base_url: &str,
    openapi_json: &str,
    candidates: &WorkflowManifest,
    cfg: &AiConfig,
) -> Result<WorkflowManifest> {
    let index_summary = summarize_index(openapi_json);
    let candidate_json = serde_json::to_string_pretty(&candidates.workflows)
        .unwrap_or_else(|_| "[]".into());
    let user = format!(
        "Base URL: {base_url}\n\nOpenAPI index:\n```json\n{index_summary}\n```\n\nCandidate workflows to refine:\n```json\n{candidate_json}\n```\n\nReturn improved workflows JSON."
    );
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: REFINE_SYSTEM.into(),
        },
        ChatMessage {
            role: "user".into(),
            content: user,
        },
    ];

    debug!("requesting workflow refine from LLM");
    let raw = complete_chat(cfg, &messages).await?;
    parse_workflow_response(&raw, base_url)
}

/// Generate workflows from scratch (fallback when heuristics produce nothing).
pub async fn generate_workflows_from_openapi(
    base_url: &str,
    openapi_json: &str,
    cfg: &AiConfig,
) -> Result<WorkflowManifest> {
    let summary = summarize_index(openapi_json);
    let user = format!(
        "Base URL: {base_url}\n\nOpenAPI summary:\n```json\n{summary}\n```\n\nGenerate workflow scenarios JSON."
    );
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: GENERATE_SYSTEM.into(),
        },
        ChatMessage {
            role: "user".into(),
            content: user,
        },
    ];

    debug!("requesting workflow scenarios from LLM");
    let raw = complete_chat(cfg, &messages).await?;
    parse_workflow_response(&raw, base_url)
}

pub fn save_workflow_manifest(manifest: &WorkflowManifest, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut to_save = manifest.clone();
    to_save.manifest_version = MANIFEST_VERSION;
    let text = serde_json::to_string_pretty(&to_save)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn parse_workflow_response(raw: &str, base_url: &str) -> Result<WorkflowManifest> {
    let json_str = extract_json_block(raw);
    let v: Value = serde_json::from_str(&json_str).map_err(|e| {
        Error::Other(format!("LLM returned invalid workflow JSON: {e}\n---\n{json_str}"))
    })?;

    let arr = v
        .get("workflows")
        .and_then(|w| w.as_array())
        .cloned()
        .unwrap_or_default();

    if arr.is_empty() {
        return Err(Error::Other("LLM returned no workflows".into()));
    }

    let mut workflows = Vec::new();
    for item in arr {
        match serde_json::from_value::<WorkflowScenario>(item) {
            Ok(w) if !w.steps.is_empty() => workflows.push(w),
            Ok(_) => {}
            Err(e) => debug!(error = %e, "skip invalid workflow entry"),
        }
    }

    if workflows.is_empty() {
        return Err(Error::Other("LLM workflows had no valid steps".into()));
    }

    Ok(WorkflowManifest {
        manifest_version: MANIFEST_VERSION,
        base_url: base_url.to_string(),
        workflows,
    })
}

fn extract_json_block(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return trimmed.to_string();
    }
    if let Some(start) = trimmed.find("```") {
        let rest = &trimmed[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest).trim_start();
        if let Some(end) = rest.find("```") {
            return rest[..end].trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Compact OpenAPI index for LLM context.
fn summarize_index(openapi_json: &str) -> String {
    let index = OpenApiIndex::from_json(openapi_json);
    let ops: Vec<Value> = index
        .ops
        .iter()
        .take(120)
        .map(|op| {
            serde_json::json!({
                "path": op.path,
                "method": op.method,
                "tags": op.tags,
                "summary": op.summary,
                "requires_auth": op.requires_auth,
                "path_params": op.path_params.iter().map(|p| &p.name).collect::<Vec<_>>(),
            })
        })
        .collect();

    serde_json::to_string_pretty(&serde_json::json!({ "operations": ops }))
        .unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_fence() {
        let raw = "Here:\n```json\n{\"workflows\":[]}\n```";
        assert!(extract_json_block(raw).contains("workflows"));
    }
}
