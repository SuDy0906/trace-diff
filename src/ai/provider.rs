//! LLM provider adapters: Ollama (local) and Groq (free cloud).

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProvider {
    Auto,
    Ollama,
    Groq,
}

impl AiProvider {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "ollama" | "local" => Ok(Self::Ollama),
            "groq" => Ok(Self::Groq),
            other => Err(Error::Other(format!(
                "unknown AI provider '{other}' (use auto, ollama, or groq)"
            ))),
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Auto | Self::Groq => "llama-3.1-8b-instant",
            Self::Ollama => "qwen2.5:7b-instruct",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ollama => "ollama",
            Self::Groq => "groq",
        }
    }
}

/// Concrete provider chosen after `auto` resolution (or explicit selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedProvider {
    Groq,
    Ollama,
}

impl ResolvedProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Groq => "groq",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub provider: AiProvider,
    pub resolved: ResolvedProvider,
    pub model: String,
    pub ollama_host: String,
    pub groq_api_key: Option<String>,
    pub timeout_secs: u64,
}

impl AiConfig {
    pub fn resolve_model(&self) -> &str {
        &self.model
    }

    pub fn active_label(&self) -> &str {
        self.resolved.label()
    }
}

#[derive(Debug, Clone)]
pub struct AiResolution {
    pub requested: AiProvider,
    pub config: Option<AiConfig>,
    pub groq_key_set: bool,
    pub ollama_reachable: bool,
    pub ollama_host: String,
}

impl AiResolution {
    pub fn resolved_label(&self) -> &str {
        match &self.config {
            Some(c) => c.active_label(),
            None if self.requested == AiProvider::Auto => "none",
            None => self.requested.label(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.config.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub async fn ollama_reachable(host: &str) -> bool {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    let client = match http_client(3) {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

pub fn groq_key_set(key: Option<&str>) -> bool {
    key.map(|k| !k.trim().is_empty()).unwrap_or(false)
}

/// Resolve CLI/env AI settings into an optional ready-to-use config.
pub async fn resolve_ai_config(
    provider_str: &str,
    model: Option<String>,
    ollama_host: String,
    groq_api_key: Option<String>,
    timeout_secs: u64,
) -> Result<AiResolution> {
    let requested = AiProvider::parse(provider_str)?;
    let groq_ok = groq_key_set(groq_api_key.as_deref());
    let ollama_ok = ollama_reachable(&ollama_host).await;

    let resolved = match requested {
        AiProvider::Auto => {
            if groq_ok {
                Some(ResolvedProvider::Groq)
            } else if ollama_ok {
                Some(ResolvedProvider::Ollama)
            } else {
                None
            }
        }
        AiProvider::Groq if groq_ok => Some(ResolvedProvider::Groq),
        AiProvider::Ollama if ollama_ok => Some(ResolvedProvider::Ollama),
        AiProvider::Groq | AiProvider::Ollama => None,
    };

    let config = if let Some(r) = resolved {
        let default_model = match r {
            ResolvedProvider::Groq => AiProvider::Groq.default_model(),
            ResolvedProvider::Ollama => AiProvider::Ollama.default_model(),
        };
        let mut cfg = AiConfig {
            provider: requested,
            resolved: r,
            model: model.unwrap_or_else(|| default_model.to_string()),
            ollama_host: ollama_host.clone(),
            groq_api_key: groq_api_key.clone(),
            timeout_secs,
        };
        if cfg.resolved == ResolvedProvider::Ollama {
            cfg.model = resolve_ollama_model(&cfg).await.unwrap_or(cfg.model);
        }
        Some(cfg)
    } else {
        None
    };

    Ok(AiResolution {
        requested,
        config,
        groq_key_set: groq_ok,
        ollama_reachable: ollama_ok,
        ollama_host,
    })
}

pub fn print_llm_check(resolution: &AiResolution) {
    let report = llm_check_report(resolution);
    if resolution.requested == AiProvider::Auto {
        println!("  auto resolved: {}", report.resolved_provider);
    } else {
        println!("  provider: {}", report.requested_provider);
    }

    if let Some(model) = &report.model {
        println!("  model: {model}");
        println!("  status: ready ({})", report.resolved_provider);
    } else {
        if resolution.requested == AiProvider::Auto || resolution.requested == AiProvider::Ollama {
            if report.ollama_reachable {
                println!("  ollama: reachable at {}", report.ollama_host);
            } else {
                println!("  ollama: unreachable at {}", report.ollama_host);
            }
        }
        if resolution.requested == AiProvider::Auto || resolution.requested == AiProvider::Groq {
            if report.groq_key_set {
                println!("  groq: GROQ_API_KEY set");
            } else {
                println!("  groq: GROQ_API_KEY not set");
            }
        }
        println!("  status: {}", report.status);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmCheckReport {
    pub requested_provider: String,
    pub resolved_provider: String,
    pub ready: bool,
    pub model: Option<String>,
    pub groq_key_set: bool,
    pub ollama_reachable: bool,
    pub ollama_host: String,
    pub status: String,
}

pub fn llm_check_report(resolution: &AiResolution) -> LlmCheckReport {
    let ready = resolution.is_ready();
    LlmCheckReport {
        requested_provider: resolution.requested.label().to_string(),
        resolved_provider: resolution.resolved_label().to_string(),
        ready,
        model: resolution
            .config
            .as_ref()
            .map(|c| c.resolve_model().to_string()),
        groq_key_set: resolution.groq_key_set,
        ollama_reachable: resolution.ollama_reachable,
        ollama_host: resolution.ollama_host.clone(),
        status: if ready {
            format!("ready ({})", resolution.resolved_label())
        } else {
            "heuristics only — see docs/LLM_SETUP.md".into()
        },
    }
}

pub fn print_llm_check_json(resolution: &AiResolution) -> Result<()> {
    let report = llm_check_report(resolution);
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| Error::Other(e.to_string()))?
    );
    Ok(())
}

pub fn llm_unavailable_hint(resolution: &AiResolution) -> Option<String> {
    if resolution.is_ready() {
        return None;
    }
    Some(
        "Optional LLM: set GROQ_API_KEY (https://console.groq.com) or run Ollama — see docs/LLM_SETUP.md"
            .into(),
    )
}

pub async fn llm_available(cfg: &AiConfig) -> bool {
    match cfg.resolved {
        ResolvedProvider::Ollama => ollama_reachable(&cfg.ollama_host).await,
        ResolvedProvider::Groq => groq_key_set(cfg.groq_api_key.as_deref()),
    }
}

pub async fn complete_chat(cfg: &AiConfig, messages: &[ChatMessage]) -> Result<String> {
    match cfg.resolved {
        ResolvedProvider::Ollama => ollama_chat(cfg, cfg.resolve_model(), messages).await,
        ResolvedProvider::Groq => groq_chat(cfg, messages).await,
    }
}

/// Pick an installed Ollama model (falls back when the default is not pulled).
pub async fn resolve_ollama_model(cfg: &AiConfig) -> Result<String> {
    let installed = ollama_list_models(cfg).await?;
    if installed.is_empty() {
        return Err(Error::Other(
            "no Ollama models installed — run: ollama pull llama3.2".into(),
        ));
    }

    let wanted = cfg.resolve_model();
    if installed.iter().any(|m| m == wanted) {
        return Ok(wanted.to_string());
    }

    // Prefix match (e.g. qwen2.5:7b matches qwen2.5:7b-instruct).
    if let Some(m) = installed
        .iter()
        .find(|m| m.starts_with(wanted) || wanted.starts_with(m.as_str()))
    {
        eprintln!("Note: using Ollama model '{m}' ({} not installed)", wanted);
        return Ok(m.clone());
    }

    for pref in OLLAMA_MODEL_PREFS {
        if installed.iter().any(|m| m.starts_with(pref)) {
            let m = installed
                .iter()
                .find(|m| m.starts_with(pref))
                .cloned()
                .unwrap();
            eprintln!("Note: using Ollama model '{m}' ({} not installed)", wanted);
            return Ok(m);
        }
    }

    let m = installed[0].clone();
    eprintln!(
        "Note: using Ollama model '{m}' ({} not installed; installed: {})",
        wanted,
        installed.join(", ")
    );
    Ok(m)
}

const OLLAMA_MODEL_PREFS: &[&str] = &[
    "qwen2.5", "llama3.2", "llama3.1", "llama3", "mistral", "phi3", "gemma",
];

async fn ollama_list_models(cfg: &AiConfig) -> Result<Vec<String>> {
    let url = format!("{}/api/tags", cfg.ollama_host.trim_end_matches('/'));
    let client = http_client(cfg.timeout_secs)?;
    let resp = client.get(&url).send().await.map_err(ollama_hint)?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "Ollama /api/tags HTTP {}",
            resp.status()
        )));
    }
    let parsed: OllamaTagsResponse = resp.json().await.map_err(|e| Error::Other(e.to_string()))?;
    Ok(parsed.models.into_iter().map(|m| m.name).collect())
}

async fn ollama_chat(cfg: &AiConfig, model: &str, messages: &[ChatMessage]) -> Result<String> {
    let url = format!("{}/api/chat", cfg.ollama_host.trim_end_matches('/'));
    let client = http_client(cfg.timeout_secs)?;
    let body = OllamaChatRequest {
        model: model.to_string(),
        messages: messages.to_vec(),
        stream: false,
    };

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(ollama_hint)?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let installed = ollama_list_models(cfg).await.unwrap_or_default();
        let hint = if installed.is_empty() {
            format!("run `ollama pull {}`", cfg.resolve_model())
        } else {
            format!(
                "installed: {} — or run `ollama pull {}`",
                installed.join(", "),
                cfg.resolve_model()
            )
        };
        return Err(Error::Other(format!(
            "Ollama error HTTP {status}: {text}\nHint: {hint}"
        )));
    }

    let parsed: OllamaChatResponse = resp.json().await.map_err(|e| Error::Other(e.to_string()))?;
    Ok(parsed.message.content.trim().to_string())
}

async fn groq_chat(cfg: &AiConfig, messages: &[ChatMessage]) -> Result<String> {
    let key = cfg
        .groq_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            Error::Other("Groq requires GROQ_API_KEY (free at https://console.groq.com)".into())
        })?;

    let client = http_client(cfg.timeout_secs)?;
    let body = OpenAiChatRequest {
        model: cfg.resolve_model().to_string(),
        messages: messages.to_vec(),
        temperature: 0.2,
    };

    let resp = client
        .post("https://api.groq.com/openai/v1/chat/completions")
        .header("Authorization", format!("Bearer {key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Other(format!("Groq request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(Error::Other(format!("Groq error HTTP {status}: {text}")));
    }

    let parsed: OpenAiChatResponse = resp.json().await.map_err(|e| Error::Other(e.to_string()))?;
    parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Other("Groq returned empty response".into()))
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

fn ollama_hint(e: reqwest::Error) -> Error {
    Error::Other(format!(
        "Could not reach Ollama ({e}). Start Ollama and run: ollama pull qwen2.5:7b-instruct"
    ))
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaTagModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagModel {
    name: String,
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    content: String,
}

#[derive(Debug, Serialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_auto_provider() {
        assert_eq!(AiProvider::parse("auto").unwrap(), AiProvider::Auto);
        assert_eq!(AiProvider::parse("AUTO").unwrap(), AiProvider::Auto);
        assert_eq!(AiProvider::parse("groq").unwrap(), AiProvider::Groq);
        assert!(AiProvider::parse("unknown").is_err());
    }

    #[test]
    fn groq_key_set_detects_empty() {
        assert!(!groq_key_set(None));
        assert!(!groq_key_set(Some("")));
        assert!(!groq_key_set(Some("   ")));
        assert!(groq_key_set(Some("gsk_test")));
    }

    #[tokio::test]
    async fn auto_resolves_ollama_when_mock_reachable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"models":[{"name":"llama3.2"}]}"#),
            )
            .mount(&server)
            .await;

        let res = resolve_ai_config("auto", None, server.uri(), None, 5)
            .await
            .unwrap();
        assert!(res.is_ready());
        assert_eq!(
            res.config.as_ref().unwrap().resolved,
            ResolvedProvider::Ollama
        );
    }

    #[tokio::test]
    async fn llm_check_report_json_fields() {
        let res = resolve_ai_config("auto", None, "http://127.0.0.1:1".into(), None, 2)
            .await
            .unwrap();
        let report = llm_check_report(&res);
        assert!(!report.ready);
        assert_eq!(report.requested_provider, "auto");
        assert!(!report.groq_key_set || report.ready);
    }
}
