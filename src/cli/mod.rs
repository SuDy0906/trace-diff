//! CLI argument parsing and command dispatch.

pub mod baseline;
pub mod diff_cmd;
pub mod features_cmd;
pub mod list;
pub mod run;

use crate::theme::ThemeName;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Parser)]
#[command(
    name = "trace-diff",
    about = "L3/L4 traceroute + L7 HTTP lifecycle diagnostics with baseline diffs",
    version
)]
pub struct Cli {
    /// Increase log verbosity (-v = debug for trace_diff, -vv = trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Alias for maximum verbosity (trace-level logs)
    #[arg(long, global = true)]
    pub debug: bool,

    /// Disable ANSI colors (also honors NO_COLOR)
    #[arg(long, global = true, env = "TRACE_DIFF_NO_COLOR")]
    pub no_color: bool,

    /// Force colors even when NO_COLOR is set
    #[arg(long, global = true)]
    pub force_color: bool,

    /// TUI color theme
    #[arg(long, global = true, value_enum, default_value_t = ThemeName::Default, env = "TRACE_DIFF_THEME")]
    pub theme: ThemeName,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a full L3/L4 + L7 probe against a target
    Run(RunArgs),
    /// Auto-detect site features, prompt, and test them (interactive)
    Features(FeaturesArgs),
    /// Manage named baselines
    Baseline(BaselineArgs),
    /// Diff the latest run (or a new probe) against a baseline
    Diff(DiffArgs),
    /// List stored runs and baselines
    List(ListArgs),
}

#[derive(Debug, Clone, Parser)]
pub struct FeaturesArgs {
    /// Site root URL or hostname (e.g. https://confuciusai.io). Omit with --check-llm.
    pub target: Option<String>,

    /// Skip interactive UI: test discovered page/API features automatically
    #[arg(long, short = 'y')]
    pub yes_all: bool,

    /// Emit JSON (with --yes-all)
    #[arg(long)]
    pub json: bool,

    /// Cap how many features to auto-run with --yes-all
    #[arg(long)]
    pub max_features: Option<u32>,

    /// Per-feature HTTP timeout
    #[arg(long, value_parser = parse_duration)]
    pub timeout: Option<Duration>,

    /// JSON file listing API routes to test (see docs/FEATURES_AUTODETECT.md)
    #[arg(long)]
    pub manifest: Option<std::path::PathBuf>,

    /// Bearer token for authenticated API probes (or set TRACE_DIFF_BEARER_TOKEN)
    #[arg(long, env = "TRACE_DIFF_BEARER_TOKEN")]
    pub bearer_token: Option<String>,

    /// Login email for FLOW auth (or TRACE_DIFF_EMAIL / CONFUCIUS_EMAIL)
    #[arg(long, env = "TRACE_DIFF_EMAIL")]
    pub email: Option<String>,

    /// Login password for FLOW auth (or TRACE_DIFF_PASSWORD / CONFUCIUS_PASSWORD)
    #[arg(long, env = "TRACE_DIFF_PASSWORD")]
    pub password: Option<String>,

    /// JSON auth profile (single user or multi-profile — see docs/FEATURES_AUTODETECT.md)
    #[arg(long)]
    pub auth_file: Option<std::path::PathBuf>,

    /// Include mutating write-smoke FLOWs (off by default in CI)
    #[arg(long)]
    pub include_writes: bool,

    /// Treat yellow Reachable rows as CI failure (with --yes-all)
    #[arg(long)]
    pub fail_on_reachable: bool,

    /// Fail if any selected probe TTFB exceeds this duration (with --yes-all)
    #[arg(long, value_parser = parse_duration)]
    pub fail_if_ttfb_exceeds: Option<Duration>,

    /// Days before certificate expiry to warn (yellow) on the TLS canary
    #[arg(long, default_value_t = 21)]
    pub cert_warn_days: i64,

    /// Skip the TLS + certificate canary
    #[arg(long)]
    pub no_tls_canary: bool,

    /// Skip LLM workflow inference (use flat OpenAPI endpoint list)
    #[arg(long)]
    pub no_llm: bool,

    /// Print LLM provider status and exit (for pip setup verification)
    #[arg(long)]
    pub check_llm: bool,

    /// LLM provider for background workflow inference: auto, groq, or ollama
    #[arg(long, env = "TRACE_DIFF_AI_PROVIDER", default_value = "auto")]
    pub llm_provider: String,

    #[arg(long, env = "TRACE_DIFF_AI_MODEL")]
    pub llm_model: Option<String>,

    #[arg(long, env = "OLLAMA_HOST", default_value = "http://localhost:11434")]
    pub ollama_host: String,

    #[arg(long, env = "GROQ_API_KEY")]
    pub groq_api_key: Option<String>,

    #[arg(long, default_value_t = 120)]
    pub llm_timeout_secs: u64,
}

#[derive(Debug, Clone, Parser)]
pub struct RunArgs {
    /// Target URL or hostname (e.g. https://api.example.com or 8.8.8.8)
    pub target: String,

    /// Save this run as a named baseline
    #[arg(long)]
    pub save_baseline: Option<String>,

    /// Immediately compare against an existing baseline after the run
    #[arg(long)]
    pub compare_baseline: Option<String>,

    /// Maximum TTL / hop count
    #[arg(long, default_value_t = 30)]
    pub max_ttl: u8,

    /// Probes per hop
    #[arg(long, default_value_t = 3)]
    pub probes: u8,

    /// Per-probe timeout
    #[arg(long, default_value = "2s", value_parser = parse_duration)]
    pub timeout: Duration,

    /// Skip L3/L4 traceroute
    #[arg(long)]
    pub skip_trace: bool,

    /// Skip L7 HTTP probe
    #[arg(long)]
    pub skip_http: bool,

    /// Emit JSON to stdout (headless / CI)
    #[arg(long, value_enum, default_value_t = OutputFormat::Tui)]
    pub output: OutputFormat,

    /// Alias for `--output json`
    #[arg(long)]
    pub headless: bool,

    /// Fail if TTFB exceeds this duration
    #[arg(long, value_parser = parse_duration)]
    pub fail_if_ttfb_exceeds: Option<Duration>,

    /// Fail if TCP handshake exceeds this duration
    #[arg(long, value_parser = parse_duration)]
    pub fail_if_handshake_exceeds: Option<Duration>,

    /// Fail if DNS resolution exceeds this duration
    #[arg(long, value_parser = parse_duration)]
    pub fail_if_dns_exceeds: Option<Duration>,

    /// Path to SQLite database (default: platform data dir)
    #[arg(long, env = "TRACE_DIFF_DB")]
    pub db: Option<PathBuf>,

    /// L3/L4 probe strategy: auto tries ICMP then TCP/UDP for silent hops
    #[arg(long, value_enum, default_value_t = ProbeArg::Auto)]
    pub probe: ProbeArg,

    /// Destination port for TCP path probes (default: from URL, else 443)
    #[arg(long)]
    pub probe_port: Option<u16>,

    /// Skip reverse-DNS / ASN enrichment on hops
    #[arg(long)]
    pub no_enrich: bool,

    /// Number of Paris traceroute flows to merge (detailed path)
    #[arg(long, default_value_t = 2)]
    pub probe_flows: u8,
}

#[derive(Debug, Clone, Parser)]
pub struct BaselineArgs {
    #[command(subcommand)]
    pub action: BaselineAction,

    #[arg(long, env = "TRACE_DIFF_DB")]
    pub db: Option<PathBuf>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum BaselineAction {
    /// Tag an existing run id as a named baseline
    Tag { run_id: String, name: String },
    /// Delete a named baseline
    Delete { name: String },
    /// Show baseline details
    Show { name: String },
}

#[derive(Debug, Clone, Parser)]
pub struct DiffArgs {
    /// Baseline name to compare against
    pub baseline: String,

    /// Optional target to re-probe; if omitted, diffs against the latest run
    pub target: Option<String>,

    #[arg(long, value_enum, default_value_t = OutputFormat::Tui)]
    pub output: OutputFormat,

    #[arg(long)]
    pub headless: bool,

    #[arg(long)]
    pub skip_trace: bool,

    #[arg(long)]
    pub skip_http: bool,

    #[arg(long, env = "TRACE_DIFF_DB")]
    pub db: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct ListArgs {
    #[arg(long, env = "TRACE_DIFF_DB")]
    pub db: Option<PathBuf>,

    #[arg(long)]
    pub baselines_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputFormat {
    #[default]
    Tui,
    Json,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ProbeArg {
    /// ICMP first; fill silent hops with TCP/UDP when elevated
    #[default]
    Auto,
    Icmp,
    Udp,
    Tcp,
}

impl ProbeArg {
    pub fn to_probe_kind(self) -> crate::traceroute::ProbeKind {
        match self {
            Self::Auto => crate::traceroute::ProbeKind::Auto,
            Self::Icmp => crate::traceroute::ProbeKind::Icmp,
            Self::Udp => crate::traceroute::ProbeKind::Udp,
            Self::Tcp => crate::traceroute::ProbeKind::Tcp,
        }
    }
}

pub fn parse_duration(s: &str) -> Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| e.to_string())
}

pub fn effective_output(format: OutputFormat, headless: bool) -> OutputFormat {
    if headless {
        OutputFormat::Json
    } else {
        format
    }
}
