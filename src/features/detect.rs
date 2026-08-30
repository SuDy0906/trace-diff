//! Discover candidate features from a site root URL.

use super::openapi_index::OpenApiIndex;
use super::workflow::{self, validate_and_filter_workflows, workflow_to_feature, MANIFEST_VERSION};
use super::{DetectedFeature, FeatureKind};
use crate::ai::{self, AiConfig, AiResolution};
use crate::error::{Error, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;
use url::Url;

#[derive(Debug, Clone, Default)]
pub struct DiscoverOptions<'a> {
    pub manifest: Option<&'a Path>,
    pub llm: Option<AiConfig>,
    pub ai_resolution: Option<AiResolution>,
    /// When true, prefer LLM workflow scenarios over flat OpenAPI endpoint lists.
    pub infer_workflows: bool,
    /// Skip inserting the TLS certificate canary row.
    pub skip_tls_canary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmDiscoveryStatus {
    Disabled,
    Unavailable { hint: String },
    Cached,
    HeuristicsOnly,
    Refined { provider: String },
    Generated { provider: String },
}

impl LlmDiscoveryStatus {
    pub fn status_suffix(&self) -> Option<String> {
        match self {
            Self::Disabled => None,
            Self::Unavailable { .. } => {
                Some("heuristics only — set GROQ_API_KEY for smarter flows".into())
            }
            Self::Cached => Some("workflows from cache".into()),
            Self::HeuristicsOnly => Some("heuristics only".into()),
            Self::Refined { provider } => Some(format!("LLM refined ({provider})")),
            Self::Generated { provider } => Some(format!("LLM generated ({provider})")),
        }
    }

    pub fn stderr_hint(&self) -> Option<&str> {
        match self {
            Self::Unavailable { hint } => Some(hint.as_str()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoverOutcome {
    pub features: Vec<DetectedFeature>,
    pub llm: LlmDiscoveryStatus,
}

const COMMON_PATHS: &[(&str, &str, FeatureKind)] = &[
    ("/", "homepage", FeatureKind::Page),
    ("/health", "health", FeatureKind::Api),
    ("/api/health", "api_health", FeatureKind::Api),
    ("/v1/health", "v1_health", FeatureKind::Api),
    ("/api/v1/health", "api_v1_health", FeatureKind::Api),
    ("/ready", "ready", FeatureKind::Api),
    ("/status", "status", FeatureKind::Api),
    ("/robots.txt", "robots", FeatureKind::Meta),
    ("/sitemap.xml", "sitemap", FeatureKind::Meta),
    ("/favicon.ico", "favicon", FeatureKind::Meta),
];

const API_EXTRA_PATHS: &[(&str, &str, FeatureKind)] = &[
    ("/v1", "v1", FeatureKind::Api),
    ("/v2", "v2", FeatureKind::Api),
    ("/api", "api", FeatureKind::Api),
    ("/api/v1", "api_v1", FeatureKind::Api),
    ("/graphql", "graphql", FeatureKind::Api),
    ("/metrics", "metrics", FeatureKind::Api),
    ("/live", "live", FeatureKind::Api),
    ("/ping", "ping", FeatureKind::Api),
];

const OPENAPI_PATHS_API_HOST: &[&str] = &[
    "/openapi.json",
    "/swagger.json",
    "/api/openapi.json",
    "/v3/openapi.json",
];

/// Max time to wait for LLM during discovery (heuristic results show first).
const DISCOVERY_LLM_TIMEOUT_SECS: u64 = 20;

const OPENAPI_PATHS: &[&str] = &[
    "/openapi.json",
    "/openapi.yaml",
    "/openapi.yml",
    "/swagger.json",
    "/swagger/v1/swagger.json",
    "/api/openapi.json",
    "/api/swagger.json",
    "/api-docs",
    "/v1/openapi.json",
    "/v2/openapi.json",
    "/v3/openapi.json",
    "/docs/openapi.json",
    "/.well-known/openapi.json",
];

/// Auto-detect pages, workflows, and common API paths for `base`.
pub async fn discover_features(base: &str, opts: DiscoverOptions<'_>) -> Result<DiscoverOutcome> {
    let mut llm_status = initial_llm_status(&opts);
    let root = normalize_base(base)?;
    let api_host = is_api_host(&root);
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("trace-diff-features/0.1")
        .build()
        .map_err(|e| Error::Other(e.to_string()))?;

    let mut by_url: BTreeMap<String, DetectedFeature> = BTreeMap::new();
    let mut used_workflows = false;

    // User-supplied manifest (workflow JSON or endpoint list).
    if let Some(path) = opts.manifest {
        if let Ok(workflows) = workflow::load_workflow_manifest(path, base) {
            for feat in workflows {
                insert(&mut by_url, feat);
            }
            used_workflows = true;
        } else {
            for feat in load_manifest(path, &root)? {
                insert(&mut by_url, feat);
            }
        }
    }

    let openapi_text = fetch_openapi_spec(&client, &root, api_host).await;

    // Workflows: cached manifest → heuristics → validate → LLM refine → cache.
    if opts.infer_workflows && !used_workflows {
        if let Some(spec) = openapi_text.as_ref() {
            let index = OpenApiIndex::from_json(spec);
            let cache_path = default_workflow_manifest_path(&root);

            if cache_path.is_file() {
                if let Ok(cached) = workflow::load_workflow_manifest(&cache_path, base) {
                    let workflow_count = cached
                        .iter()
                        .filter(|f| f.kind == FeatureKind::Workflow)
                        .count();
                    if workflow_count > 0 {
                        for feat in cached {
                            insert(&mut by_url, feat);
                        }
                        used_workflows = true;
                        llm_status = LlmDiscoveryStatus::Cached;
                        debug!(path = %cache_path.display(), "loaded cached workflow manifest");
                    }
                }
            }

            if !used_workflows {
                let scenarios = workflow::heuristic_workflows_from_openapi(spec);
                let mut manifest = workflow::WorkflowManifest {
                    manifest_version: MANIFEST_VERSION,
                    base_url: base.to_string(),
                    workflows: scenarios,
                };

                if !manifest.workflows.is_empty() {
                    let mut llm_used = false;
                    if let Some(cfg) = opts.llm.as_ref() {
                        if ai::llm_available(cfg).await {
                            let mut llm_cfg = cfg.clone();
                            llm_cfg.timeout_secs =
                                llm_cfg.timeout_secs.min(DISCOVERY_LLM_TIMEOUT_SECS);
                            match ai::refine_workflows_from_openapi(base, spec, &manifest, &llm_cfg)
                                .await
                            {
                                Ok(refined) => {
                                    let validated =
                                        validate_and_filter_workflows(refined.workflows, &index);
                                    if !validated.is_empty() {
                                        manifest.workflows = validated;
                                        llm_used = true;
                                        llm_status = LlmDiscoveryStatus::Refined {
                                            provider: cfg.active_label().to_string(),
                                        };
                                        debug!(
                                            workflows = manifest.workflows.len(),
                                            "LLM refined workflows"
                                        );
                                    }
                                }
                                Err(e) => {
                                    debug!(error = %e, "LLM refine skipped — using heuristics");
                                }
                            }
                        }
                    }
                    if !llm_used && !matches!(llm_status, LlmDiscoveryStatus::Unavailable { .. }) {
                        llm_status = LlmDiscoveryStatus::HeuristicsOnly;
                    }

                    let path = default_workflow_manifest_path(&root);
                    if let Err(e) = ai::save_workflow_manifest(&manifest, &path) {
                        debug!(error = %e, "could not save workflow manifest");
                    }
                    for w in &manifest.workflows {
                        insert(&mut by_url, workflow_to_feature(base, w));
                    }
                    used_workflows = true;
                    debug!(
                        workflows = manifest.workflows.len(),
                        "workflows from OpenAPI pipeline"
                    );
                } else if let Some(cfg) = opts.llm.as_ref() {
                    if ai::llm_available(cfg).await {
                        let mut llm_cfg = cfg.clone();
                        llm_cfg.timeout_secs = llm_cfg.timeout_secs.min(DISCOVERY_LLM_TIMEOUT_SECS);
                        match ai::generate_workflows_from_openapi(base, spec, &llm_cfg).await {
                            Ok(refined) => {
                                let validated =
                                    validate_and_filter_workflows(refined.workflows, &index);
                                if !validated.is_empty() {
                                    let manifest = workflow::WorkflowManifest {
                                        manifest_version: MANIFEST_VERSION,
                                        base_url: base.to_string(),
                                        workflows: validated,
                                    };
                                    let path = default_workflow_manifest_path(&root);
                                    let _ = ai::save_workflow_manifest(&manifest, &path);
                                    for w in &manifest.workflows {
                                        insert(&mut by_url, workflow_to_feature(base, w));
                                    }
                                    used_workflows = true;
                                    llm_status = LlmDiscoveryStatus::Generated {
                                        provider: cfg.active_label().to_string(),
                                    };
                                    debug!(workflows = manifest.workflows.len(), "LLM workflows");
                                }
                            }
                            Err(e) => debug!(error = %e, "LLM workflow inference unavailable"),
                        }
                    } else if !matches!(llm_status, LlmDiscoveryStatus::Unavailable { .. }) {
                        llm_status = LlmDiscoveryStatus::HeuristicsOnly;
                    }
                }
            }
        } else if api_host {
            for feat in workflow::default_smoke_workflows(base) {
                insert(&mut by_url, feat);
            }
            used_workflows = true;
        }
    }

    if !used_workflows {
        insert(
            &mut by_url,
            DetectedFeature {
                id: "homepage".into(),
                label: if api_host {
                    "API root".into()
                } else {
                    "Homepage".into()
                },
                url: root.to_string(),
                kind: if api_host {
                    FeatureKind::Api
                } else {
                    FeatureKind::Page
                },
                source: "root".into(),
                method: None,
                workflow: None,
            },
        );

        if let Some(text) = &openapi_text {
            for feat in parse_openapi_body(text, &root) {
                insert(&mut by_url, feat);
            }
        } else {
            for feat in discover_openapi(&client, &root).await {
                insert(&mut by_url, feat);
            }
        }
    } else {
        // Always keep lightweight health checks alongside workflows.
        for (path, id, _) in [
            ("/health", "health", FeatureKind::Api),
            ("/api/health", "api_health", FeatureKind::Api),
        ] {
            let url = join_path(&root, path);
            insert(
                &mut by_url,
                DetectedFeature {
                    id: id.to_string(),
                    label: human_label(id),
                    url,
                    kind: FeatureKind::Api,
                    source: "common_path".into(),
                    method: None,
                    workflow: None,
                },
            );
        }
    }

    // Fetch root — HTML links or JSON hints (skip when LLM workflows cover API host).
    if !used_workflows {
        let home = fetch_response(&client, root.as_str()).await;
        if let Some((body, content_type)) = home {
            if looks_like_json(&content_type, &body) {
                for feat in parse_json_api_hints(&body, &root) {
                    insert(&mut by_url, feat);
                }
            } else {
                for href in extract_hrefs(&body) {
                    if let Some(feat) = link_to_feature(&root, &href) {
                        insert(&mut by_url, feat);
                    }
                }
                for href in extract_hrefs(&body) {
                    if href.contains("openapi") || href.contains("swagger") {
                        if let Ok(spec_url) = root.join(&href) {
                            if let Some(text) = fetch_text(&client, spec_url.as_str()).await {
                                for feat in parse_openapi_body(&text, &root) {
                                    insert(&mut by_url, feat);
                                }
                            }
                        }
                    }
                }
            }
        }

        let paths_to_probe: Vec<(&str, &str, FeatureKind)> = if api_host {
            COMMON_PATHS
                .iter()
                .chain(API_EXTRA_PATHS.iter())
                .copied()
                .collect()
        } else {
            COMMON_PATHS.to_vec()
        };

        for (path, id, kind) in paths_to_probe {
            if path == "/" {
                continue;
            }
            if api_host && matches!(kind, FeatureKind::Meta) && id != "sitemap" {
                continue;
            }
            let url = join_path(&root, path);
            match client.get(&url).send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if route_exists_status(status) {
                        insert(
                            &mut by_url,
                            DetectedFeature {
                                id: id.to_string(),
                                label: human_label(id),
                                url,
                                kind,
                                source: format!("common_path ({status})"),
                                method: None,
                                workflow: None,
                            },
                        );
                    }
                }
                Err(e) => debug!(%url, error = %e, "common path probe failed"),
            }
        }

        if !api_host {
            if let Some(sitemap_url) = by_url
                .values()
                .find(|f| f.id == "sitemap")
                .map(|f| f.url.clone())
            {
                if let Some(xml) = fetch_text(&client, &sitemap_url).await {
                    for loc in extract_sitemap_locs(&xml).into_iter().take(40) {
                        if let Ok(u) = Url::parse(&loc) {
                            if u.host_str() == root.host_str() {
                                let id = path_to_id(u.path());
                                insert(
                                    &mut by_url,
                                    DetectedFeature {
                                        id: id.clone(),
                                        label: human_label(&id),
                                        url: loc,
                                        kind: FeatureKind::Page,
                                        source: "sitemap".into(),
                                        method: None,
                                        workflow: None,
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    if !opts.skip_tls_canary && root.scheme() == "https" {
        insert(&mut by_url, tls_canary_feature(&root));
    }

    let cap = if used_workflows {
        56
    } else if api_host {
        80
    } else {
        40
    };
    let mut list: Vec<_> = by_url.into_values().collect();
    list.sort_by(|a, b| {
        source_rank(&a.source)
            .cmp(&source_rank(&b.source))
            .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
            .then_with(|| a.label.cmp(&b.label))
    });
    if list.len() > cap {
        // Keep TLS even when the list is truncated.
        let tls: Vec<_> = list
            .iter()
            .filter(|f| f.kind == FeatureKind::Tls)
            .cloned()
            .collect();
        list.retain(|f| f.kind != FeatureKind::Tls);
        list.truncate(cap.saturating_sub(tls.len()));
        let mut kept = tls;
        kept.append(&mut list);
        list = kept;
        list.sort_by(|a, b| {
            source_rank(&a.source)
                .cmp(&source_rank(&b.source))
                .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
                .then_with(|| a.label.cmp(&b.label))
        });
    }
    Ok(DiscoverOutcome {
        features: list,
        llm: llm_status,
    })
}

fn initial_llm_status(opts: &DiscoverOptions<'_>) -> LlmDiscoveryStatus {
    if !opts.infer_workflows {
        return LlmDiscoveryStatus::Disabled;
    }
    if opts.llm.is_some() {
        return LlmDiscoveryStatus::HeuristicsOnly;
    }
    let hint = opts
        .ai_resolution
        .as_ref()
        .and_then(ai::llm_unavailable_hint)
        .unwrap_or_else(|| {
            "set GROQ_API_KEY (https://console.groq.com) or run Ollama — see docs/LLM_SETUP.md"
                .into()
        });
    LlmDiscoveryStatus::Unavailable { hint }
}

fn is_api_host(root: &Url) -> bool {
    root.host_str()
        .map(|h| h.starts_with("api.") || h.contains(".api."))
        .unwrap_or(false)
}

fn route_exists_status(status: u16) -> bool {
    status < 400 || status == 401 || status == 403 || status == 405
}

fn source_rank(source: &str) -> u8 {
    if source == "workflow" {
        0
    } else if source == "tls" {
        1
    } else if source == "manifest" {
        2
    } else if source == "openapi" {
        3
    } else if source.starts_with("common_path") {
        4
    } else if source == "html_link" {
        5
    } else if source == "workflow-write" {
        6
    } else {
        7
    }
}

async fn fetch_openapi_spec(client: &Client, root: &Url, api_host: bool) -> Option<String> {
    let fast = Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("trace-diff-features/0.1")
        .build()
        .ok()?;

    let paths: &[&str] = if api_host {
        OPENAPI_PATHS_API_HOST
    } else {
        OPENAPI_PATHS
    };

    for path in paths {
        let url = join_path(root, path);
        if let Some(body) = fetch_text(&fast, &url).await {
            if valid_openapi_json(&body) {
                return Some(body);
            }
        }
    }

    if api_host {
        for path in OPENAPI_PATHS {
            if OPENAPI_PATHS_API_HOST.contains(path) {
                continue;
            }
            let url = join_path(root, path);
            if let Some(body) = fetch_text(client, &url).await {
                if valid_openapi_json(&body) {
                    return Some(body);
                }
            }
        }
    }

    None
}

fn valid_openapi_json(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("paths").cloned())
        .is_some()
}

fn default_workflow_manifest_path(root: &Url) -> PathBuf {
    let host = root.host_str().unwrap_or("site").replace('.', "-");
    PathBuf::from(".trace-diff").join(format!("workflows-{host}.json"))
}

async fn discover_openapi(client: &Client, root: &Url) -> Vec<DetectedFeature> {
    for path in OPENAPI_PATHS {
        let url = join_path(root, path);
        if let Some(body) = fetch_text(client, &url).await {
            let feats = parse_openapi_body(&body, root);
            if !feats.is_empty() {
                return feats;
            }
        }
    }
    Vec::new()
}

fn parse_openapi_body(body: &str, root: &Url) -> Vec<DetectedFeature> {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    parse_openapi_value(&v, root)
}

fn parse_openapi_value(v: &Value, root: &Url) -> Vec<DetectedFeature> {
    let paths = match v.get("paths").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for (path, ops) in paths {
        let Some(ops) = ops.as_object() else {
            continue;
        };
        let methods: Vec<&str> = ops
            .keys()
            .map(|k| k.as_str())
            .filter(|k| {
                matches!(
                    *k,
                    "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
                )
            })
            .collect();
        if methods.is_empty() {
            continue;
        }

        let probe_path = templated_path_for_probe(path);
        let url = join_path(root, &probe_path);
        let id = path_to_id(path);
        let primary = methods
            .iter()
            .find(|m| **m == "get")
            .or(methods.first())
            .copied()
            .unwrap_or("get");
        let summary = ops
            .get(primary)
            .and_then(|op| op.get("summary"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string());
        let label = summary.unwrap_or_else(|| {
            if methods.len() == 1 {
                format!("{} {}", primary.to_uppercase(), human_label(&id))
            } else {
                format!(
                    "{} (+{} methods)",
                    human_label(&id),
                    methods.len().saturating_sub(1)
                )
            }
        });

        out.push(DetectedFeature {
            id: format!("{id}_{primary}"),
            label,
            url,
            kind: FeatureKind::Api,
            source: "openapi".into(),
            method: Some(primary.to_uppercase()),
            workflow: None,
        });
    }
    out
}

fn parse_json_api_hints(body: &str, root: &Url) -> Vec<DetectedFeature> {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    if v.get("paths").is_some() {
        return parse_openapi_value(&v, root);
    }

    let mut out = Vec::new();
    if let Some(arr) = v.get("endpoints").and_then(|e| e.as_array()) {
        out.extend(parse_endpoint_array(arr, root, "json_root"));
    }
    if let Some(arr) = v.get("routes").and_then(|e| e.as_array()) {
        out.extend(parse_endpoint_array(arr, root, "json_root"));
    }
    out
}

fn parse_endpoint_array(arr: &[Value], root: &Url, source: &str) -> Vec<DetectedFeature> {
    let mut out = Vec::new();
    for item in arr {
        if let Some(feat) = manifest_entry_to_feature(item, root, source) {
            out.push(feat);
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    path: Option<String>,
    url: Option<String>,
    label: Option<String>,
    method: Option<String>,
}

fn load_manifest(path: &Path, root: &Url) -> Result<Vec<DetectedFeature>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Other(format!("manifest {}: {e}", path.display())))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("manifest JSON in {}: {e}", path.display())))?;

    let entries = if let Some(arr) = v.as_array() {
        arr.clone()
    } else if let Some(arr) = v.get("endpoints").and_then(|e| e.as_array()) {
        arr.clone()
    } else if let Some(arr) = v.get("routes").and_then(|e| e.as_array()) {
        arr.clone()
    } else {
        return Err(Error::Other(format!(
            "manifest {}: expected JSON array or {{\"endpoints\": [...]}}",
            path.display()
        )));
    };

    Ok(entries
        .iter()
        .filter_map(|e| manifest_entry_to_feature(e, root, "manifest"))
        .collect())
}

fn manifest_entry_to_feature(entry: &Value, root: &Url, source: &str) -> Option<DetectedFeature> {
    let parsed: ManifestEntry = serde_json::from_value(entry.clone()).ok()?;
    let url = if let Some(u) = parsed.url {
        u
    } else {
        let path = parsed.path.clone()?;
        join_path(root, &templated_path_for_probe(&path))
    };
    let path_for_id = Url::parse(&url)
        .ok()
        .map(|u| u.path().to_string())
        .or(parsed.path)
        .unwrap_or_else(|| url.clone());
    let id = path_to_id(&path_for_id);
    let label = parsed.label.unwrap_or_else(|| human_label(&id));
    Some(DetectedFeature {
        id: id.clone(),
        label,
        url,
        kind: FeatureKind::Api,
        source: source.into(),
        method: parsed.method.map(|m| m.to_uppercase()),
        workflow: None,
    })
}

fn templated_path_for_probe(path: &str) -> String {
    let mut out = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut param = String::new();
            for ch in chars.by_ref() {
                if ch == '}' {
                    break;
                }
                param.push(ch);
            }
            let placeholder = if param.to_ascii_lowercase().contains("uuid") {
                "00000000-0000-0000-0000-000000000000"
            } else {
                "1"
            };
            out.push_str(placeholder);
        } else {
            out.push(c);
        }
    }
    out
}

fn looks_like_json(content_type: &str, body: &str) -> bool {
    content_type.contains("json")
        || body.trim_start().starts_with('{')
        || body.trim_start().starts_with('[')
}

fn kind_rank(k: FeatureKind) -> u8 {
    match k {
        FeatureKind::Workflow => 0,
        FeatureKind::Tls => 1,
        FeatureKind::Api => 2,
        FeatureKind::Page => 3,
        FeatureKind::Meta => 4,
    }
}

fn feature_key(feat: &DetectedFeature) -> String {
    match feat.kind {
        FeatureKind::Workflow => format!("workflow:{}", feat.id),
        FeatureKind::Tls => format!("tls:{}", feat.id),
        _ => feat.url.clone(),
    }
}

fn tls_canary_feature(root: &Url) -> DetectedFeature {
    DetectedFeature {
        id: "tls_cert".into(),
        label: "TLS certificate".into(),
        url: root.to_string(),
        kind: FeatureKind::Tls,
        source: "tls".into(),
        method: None,
        workflow: None,
    }
}

fn insert(map: &mut BTreeMap<String, DetectedFeature>, feat: DetectedFeature) {
    map.entry(feature_key(&feat)).or_insert(feat);
}

fn normalize_base(base: &str) -> Result<Url> {
    let t = base.trim();
    let with_scheme = if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    let mut u = Url::parse(&with_scheme).map_err(|e| Error::InvalidTarget(e.to_string()))?;
    u.set_path("/");
    u.set_query(None);
    u.set_fragment(None);
    Ok(u)
}

fn join_path(root: &Url, path: &str) -> String {
    let mut u = root.clone();
    u.set_path(path);
    u.to_string()
}

async fn fetch_response(client: &Client, url: &str) -> Option<(String, String)> {
    let resp = client.get(url).send().await.ok()?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let status = resp.status();
    let body = resp.text().await.ok()?;
    if !status.is_success() && !route_exists_status(status.as_u16()) {
        return None;
    }
    Some((body, content_type))
}

async fn fetch_text(client: &Client, url: &str) -> Option<String> {
    fetch_response(client, url)
        .await
        .map(|(body, _)| body)
        .filter(|b| !b.is_empty())
}

fn extract_hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("href") {
        let start = search_from + rel;
        search_from = start + 4;
        let rest = &html[start..];
        let Some(eq) = rest.find('=') else {
            continue;
        };
        let after = rest[eq + 1..].trim_start();
        let (q, rest2) = match after.chars().next() {
            Some('"') => ('"', &after[1..]),
            Some('\'') => ('\'', &after[1..]),
            _ => continue,
        };
        if let Some(end) = rest2.find(q) {
            let href = rest2[..end].trim();
            if !href.is_empty() && !href.starts_with('#') && !href.starts_with("javascript:") {
                out.push(href.to_string());
            }
        }
        let _ = bytes;
    }
    out
}

fn extract_sitemap_locs(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = xml.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("<loc") {
        let start = search_from + rel;
        search_from = start + 4;
        if let Some(gt) = xml[start..].find('>') {
            let content_start = start + gt + 1;
            if let Some(end_rel) = lower[content_start..].find("</loc>") {
                let loc = xml[content_start..content_start + end_rel].trim();
                if loc.starts_with("http") {
                    out.push(loc.to_string());
                }
                search_from = content_start + end_rel + 6;
            }
        }
    }
    out
}

fn link_to_feature(root: &Url, href: &str) -> Option<DetectedFeature> {
    let joined = root.join(href).ok()?;
    if joined.scheme() != "http" && joined.scheme() != "https" {
        return None;
    }
    if joined.host_str()? != root.host_str()? {
        return None;
    }
    let path = joined.path();
    if path.is_empty() || path == "/" {
        return None;
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".css")
        || lower.ends_with(".js")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".svg")
        || lower.ends_with(".woff")
        || lower.ends_with(".woff2")
        || lower.ends_with(".ico")
    {
        return None;
    }
    let id = path_to_id(path);
    let kind = if path.contains("/api") || path.contains("/v1") {
        FeatureKind::Api
    } else {
        FeatureKind::Page
    };
    Some(DetectedFeature {
        id: id.clone(),
        label: human_label(&id),
        url: {
            let mut u = joined;
            u.set_fragment(None);
            u.to_string()
        },
        kind,
        source: "html_link".into(),
        method: None,
        workflow: None,
    })
}

fn path_to_id(path: &str) -> String {
    let p = path.trim_matches('/');
    if p.is_empty() {
        return "home".into();
    }
    let s: String = p
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_ascii_lowercase();
    if s.len() > 40 {
        s[..40].to_string()
    } else if s.is_empty() {
        "page".into()
    } else {
        s
    }
}

fn human_label(id: &str) -> String {
    id.split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::workflow::WorkflowScenario;
    use super::*;

    #[test]
    fn href_extract() {
        let html = r#"<a href="/pricing">P</a><a href='https://example.com/about'>A</a>"#;
        let hrefs = extract_hrefs(html);
        assert!(hrefs.iter().any(|h| h == "/pricing"));
    }

    #[test]
    fn path_id() {
        assert_eq!(path_to_id("/api/v1/health"), "api_v1_health");
    }

    #[test]
    fn api_host_detection() {
        let u = normalize_base("https://api.confuciusai.io").unwrap();
        assert!(is_api_host(&u));
    }

    #[test]
    fn openapi_paths_parsed() {
        let spec = r#"{
            "openapi": "3.0.0",
            "paths": {
                "/v1/users": { "get": { "summary": "List users" } },
                "/v1/chat": { "post": {} }
            }
        }"#;
        let root = normalize_base("https://api.example.com").unwrap();
        let feats = parse_openapi_body(spec, &root);
        assert_eq!(feats.len(), 2);
        assert!(feats.iter().any(|f| f.label == "List users"));
        assert!(feats.iter().any(|f| f.method.as_deref() == Some("POST")));
    }

    #[test]
    fn templated_path() {
        assert_eq!(
            templated_path_for_probe("/users/{id}/posts/{postId}"),
            "/users/1/posts/1"
        );
    }

    #[test]
    fn route_exists_includes_405() {
        assert!(route_exists_status(405));
        assert!(!route_exists_status(404));
    }

    #[test]
    fn workflow_features_do_not_collide_on_base_url() {
        let mut map = BTreeMap::new();
        let base = "https://api.example.com";
        for id in ["health_smoke", "auth_smoke", "admin_flow"] {
            insert(
                &mut map,
                workflow_to_feature(
                    base,
                    &WorkflowScenario {
                        id: id.into(),
                        label: id.into(),
                        description: String::new(),
                        kind: workflow::FlowKind::Read,
                        auth_realm: None,
                        steps: vec![],
                    },
                ),
            );
        }
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn tls_ranks_ahead_of_pages() {
        assert!(kind_rank(FeatureKind::Tls) < kind_rank(FeatureKind::Api));
        assert!(kind_rank(FeatureKind::Tls) < kind_rank(FeatureKind::Page));
        let root = normalize_base("https://api.example.com").unwrap();
        let tls = tls_canary_feature(&root);
        assert_eq!(tls.kind, FeatureKind::Tls);
        assert_eq!(feature_key(&tls), "tls:tls_cert");
    }
}
