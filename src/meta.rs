//! Reproducible run metadata for observability and trust.

use serde::{Deserialize, Serialize};
use std::env::consts::{ARCH, OS};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeMode {
    Full,
    L7Only,
    TraceOnly,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrivilegeLevel {
    /// Raw ICMP / elevated sockets available.
    Elevated,
    /// Unprivileged / best-effort probing only.
    Unprivileged,
    /// Privilege state unknown (not probed yet).
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMetadata {
    pub tool_version: String,
    pub os: String,
    pub arch: String,
    pub rustc_host: String,
    pub probe_mode: ProbeMode,
    pub privileges: PrivilegeLevel,
    pub skip_trace: bool,
    pub skip_http: bool,
    /// Wall-clock note: durations use monotonic Instant, not wall NTP time.
    pub timing_basis: String,
    pub timing_disclaimer: String,
}

impl RunMetadata {
    pub fn capture(skip_trace: bool, skip_http: bool, privileges: PrivilegeLevel) -> Self {
        let probe_mode = match (skip_trace, skip_http) {
            (false, false) => ProbeMode::Full,
            (true, false) => ProbeMode::L7Only,
            (false, true) => ProbeMode::TraceOnly,
            (true, true) => ProbeMode::None,
        };
        Self {
            tool_version: VERSION.to_string(),
            os: OS.to_string(),
            arch: ARCH.to_string(),
            rustc_host: format!("{ARCH}-{OS}"),
            probe_mode,
            privileges,
            skip_trace,
            skip_http,
            timing_basis: "std::time::Instant (monotonic)".into(),
            timing_disclaimer: TIMING_DISCLAIMER.into(),
        }
    }
}

/// Documented guarantee: phase durations are monotonic deltas, not NTP wall clock.
pub const TIMING_DISCLAIMER: &str = "\
Phase timings (DNS/TCP/TLS/TTFB/transfer and hop RTTs) are measured with \
std::time::Instant monotonic clocks. Absolute wall-clock timestamps (measured_at) \
come from the system clock and may jump if NTP steps the clock. Do not compare \
Instant-based durations to wall-clock intervals across machines without accounting \
for clock skew.";

/// Best-effort check: try opening an ICMP socket.
pub fn detect_icmp_privilege() -> PrivilegeLevel {
    use surge_ping::{Client, Config, ICMP};
    match Client::new(&Config::builder().kind(ICMP::V4).build()) {
        Ok(_) => PrivilegeLevel::Elevated,
        Err(_) => PrivilegeLevel::Unprivileged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_modes() {
        let m = RunMetadata::capture(true, false, PrivilegeLevel::Unprivileged);
        assert_eq!(m.probe_mode, ProbeMode::L7Only);
        assert!(!m.timing_disclaimer.is_empty());
    }
}
