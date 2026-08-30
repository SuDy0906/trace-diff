//! Error types for trace-diff.

use miette::Diagnostic;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error("invalid target URL or host: {0}")]
    InvalidTarget(String),

    #[error("DNS resolution failed for {host}: {source}")]
    Dns {
        host: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("traceroute failed: {0}")]
    Traceroute(String),

    #[error("HTTP probe failed: {0}")]
    HttpProbe(String),

    #[error("baseline '{0}' not found")]
    #[diagnostic(help("run `trace-diff list` to see available baselines"))]
    BaselineNotFound(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("threshold exceeded: {metric} = {actual}, limit = {limit}")]
    #[diagnostic(help("CI mode fails when measured latency exceeds --fail-if-* thresholds"))]
    ThresholdExceeded {
        metric: String,
        actual: String,
        limit: String,
    },

    #[error("{0}")]
    Other(String),
}
