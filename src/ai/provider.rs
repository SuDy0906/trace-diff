//! LLM provider adapters: Ollama (local) and Groq (free cloud).

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProvider {
    Ollama,
    Groq,
}

impl AiProvider {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ollama" | "local" => Ok(Self::Ollama),
            "groq" => Ok(Self::Groq),
            other => Err(Error::Other(format!(
                "unknown AI provider '{other}' (use ollama or groq)"
            ))),
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Ollama => "qwen2.5:7b-instruct",
            Self::Groq => "llama-3.1-8b-instant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiConfig {
    pub provider: AiProvider,
    pub model: String,
    pub ollama_host: String,
    pub groq_api_key: Option<String>,
    pub timeout_secs: u64,
}

impl AiConfig {
    pub fn resolve_model(&self) -> &str {
        &self.model
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub async fn llm_available(cfg: &AiConfig) -> bool {
    match cfg.provider {
        AiProvider::Ollama => {
            let url = format!("{}/api/tags", cfg.ollama_host.trim_end_matches('/'));
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
        AiProvider::Groq => cfg
            .groq_api_key
            .as_deref()
            .map(|k| !k.is_empty())
            .unwrap_or(false),
    }
}

pub async fn complete_chat(cfg: &AiConfig, messages: &[ChatMessage]) -> Result<String> {
    match cfg.provider {
        AiProvider::Ollama => ollama_chat(cfg, cfg.resolve_model(), messages).await,
        AiProvider::Groq => groq_chat(cfg, messages).await,
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
