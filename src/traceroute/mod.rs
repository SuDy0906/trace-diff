//! Paris Traceroute-style L3/L4 hop-by-hop prober.
//!
//! Supports ICMP Echo (surge-ping), plus best-effort UDP/TCP probes that
//! listen for ICMP Time Exceeded when raw sockets are available. Auto mode
//! merges replies so silent ICMP hops can still surface via UDP/TCP.

mod enrich;
mod icmp;
mod raw;

use crate::error::Result;
use crate::progress::ProgressEvent;
use crate::stats::LatencySummary;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

pub use enrich::enrich_hops;

pub type ProgressTx = UnboundedSender<ProgressEvent>;

fn emit(tx: &Option<ProgressTx>, event: ProgressEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Which probe elicited a hop reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplyProto {
    #[default]
    Icmp,
    Udp,
    Tcp,
}

impl ReplyProto {
    pub fn label(self) -> &'static str {
        match self {
            Self::Icmp => "ICMP",
            Self::Udp => "UDP",
            Self::Tcp => "TCP",
        }
    }
}

/// Probe strategy selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// Try TCP → UDP → ICMP per hop; merge any replies.
    #[default]
    Auto,
    Icmp,
    Udp,
    Tcp,
}

impl ProbeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Icmp => "icmp",
            Self::Udp => "udp",
            Self::Tcp => "tcp",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HopResult {
    pub ttl: u8,
    pub address: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_name: Option<String>,
    /// Protocol that returned the hop identity (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_proto: Option<ReplyProto>,
    /// All protocols that got a response at this TTL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protos_seen: Vec<ReplyProto>,
    pub metrics: LatencySummary,
    pub samples_ms: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathGap {
    pub from_ttl: u8,
    pub to_ttl: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathSummary {
    pub hop_count: usize,
    pub replied: usize,
    pub silent: usize,
    pub gaps: Vec<PathGap>,
    pub protocols_used: Vec<ReplyProto>,
    pub first_reply_ttl: Option<u8>,
    pub last_reply_ttl: Option<u8>,
    /// Smallest TTL that could TCP-connect to the destination (when measured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ttl_tcp_reach: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_asn: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_as_name: Option<String>,
    /// True when raw ICMP receive worked (needed for UDP/TCP hop IPs).
    #[serde(default)]
    pub raw_icmp_ok: bool,
    /// How many Paris flows were merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_count: Option<u8>,
    /// TTLs where flows disagreed on hop address.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub divergent_ttls: Vec<u8>,
}

impl PathSummary {
    pub fn from_hops(hops: &[HopResult], min_ttl_tcp_reach: Option<u8>, raw_icmp_ok: bool) -> Self {
        let hop_count = hops.len();
        let replied = hops
            .iter()
            .filter(|h| h.address.is_some() && h.metrics.recv > 0)
            .count();
        let silent = hop_count.saturating_sub(replied);

        let mut gaps = Vec::new();
        let mut gap_start: Option<u8> = None;
        for h in hops {
            let silent_hop = h.address.is_none() || h.metrics.recv == 0;
            if silent_hop {
                if gap_start.is_none() {
                    gap_start = Some(h.ttl);
                }
            } else if let Some(start) = gap_start.take() {
                gaps.push(PathGap {
                    from_ttl: start,
                    to_ttl: h.ttl.saturating_sub(1),
                });
            }
        }
        if let Some(start) = gap_start {
            if let Some(last) = hops.last() {
                gaps.push(PathGap {
                    from_ttl: start,
                    to_ttl: last.ttl,
                });
            }
        }

        let mut protocols_used = Vec::new();
        for h in hops {
            for p in &h.protos_seen {
                if !protocols_used.contains(p) {
                    protocols_used.push(*p);
                }
            }
        }

        let first_reply_ttl = hops.iter().find(|h| h.address.is_some()).map(|h| h.ttl);
        let last_reply_ttl = hops
            .iter()
            .rev()
            .find(|h| h.address.is_some())
            .map(|h| h.ttl);

        let dest = hops
            .iter()
            .rev()
            .find(|h| h.address.is_some() && h.metrics.recv > 0);

        Self {
            hop_count,
            replied,
            silent,
            gaps,
            protocols_used,
            first_reply_ttl,
            last_reply_ttl,
            min_ttl_tcp_reach,
            dest_asn: dest.and_then(|h| h.asn),
            dest_as_name: dest.and_then(|h| h.as_name.clone()),
            raw_icmp_ok,
            flow_count: None,
            divergent_ttls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceResult {
    pub target: String,
    pub resolved: IpAddr,
    pub hops: Vec<HopResult>,
    pub reached: bool,
    #[serde(default)]
    pub probe_kind: ProbeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<u16>,
    #[serde(default)]
    pub summary: PathSummary,
}

#[derive(Debug, Clone)]
pub struct TraceConfig {
    pub max_ttl: u8,
    pub probes_per_hop: u8,
    pub timeout: Duration,
    /// Fixed ICMP identifier (Paris traceroute flow affinity).
    pub icmp_id: u16,
    pub probe_kind: ProbeKind,
    /// Destination port for TCP/UDP probes (HTTPS default 443).
    pub dest_port: u16,
    /// Fixed UDP/TCP source port for Paris-style flow hashing.
    pub sport: u16,
    pub enrich: bool,
    /// Paris flows to merge for richer path detail (1 = single flow).
    pub flows: u8,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            max_ttl: 30,
            probes_per_hop: 3,
            timeout: Duration::from_secs(2),
            icmp_id: 0x7D1F,
            probe_kind: ProbeKind::Auto,
            dest_port: 443,
            sport: 44_444,
            enrich: true,
            flows: 2,
        }
    }
}

/// Resolve a host or literal IP to an address.
pub fn resolve_target(target: &str) -> Result<IpAddr> {
    icmp::resolve_target(target)
}

pub fn strip_url_host(target: &str) -> &str {
    icmp::strip_url_host(target)
}

/// Infer TCP/UDP probe port from a URL or host:port string.
pub fn infer_dest_port(target: &str) -> u16 {
    let t = target.trim();
    if let Ok(u) = url::Url::parse(t) {
        if let Some(p) = u.port() {
            return p;
        }
        return match u.scheme() {
            "http" => 80,
            _ => 443,
        };
    }
    // host:port without scheme
    let host = strip_url_host(t);
    if let Some(rest) = t.strip_prefix(host) {
        if let Some(p) = rest.strip_prefix(':') {
            let num: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(port) = num.parse() {
                return port;
            }
        }
    }
    443
}

/// Run a hop-by-hop traceroute.
pub async fn trace(target: &str, cfg: TraceConfig) -> Result<TraceResult> {
    trace_with_progress(target, cfg, None).await
}

/// Same as [`trace`], optionally streaming [`ProgressEvent`]s.
pub async fn trace_with_progress(
    target: &str,
    cfg: TraceConfig,
    progress: Option<ProgressTx>,
) -> Result<TraceResult> {
    let resolved = resolve_target(target)?;
    let flow_count = cfg.flows.max(1);
    debug!(
        %resolved,
        max_ttl = cfg.max_ttl,
        probe = cfg.probe_kind.label(),
        dest_port = cfg.dest_port,
        flows = flow_count,
        "starting traceroute"
    );
    emit(
        &progress,
        ProgressEvent::TraceStarted {
            max_ttl: cfg.max_ttl,
        },
    );

    let raw_icmp_ok = raw::raw_icmp_available(resolved);
    let tcp_ttl_ok = raw::tcp_ttl_appears_honored(resolved, &cfg).await;
    debug!(raw_icmp_ok, tcp_ttl_ok, "path probe capabilities");

    // Primary flow.
    let (mut hops, mut reached) =
        walk_path(resolved, &cfg, raw_icmp_ok, tcp_ttl_ok, &progress).await;
    let mut divergent_ttls = Vec::new();

    // Extra Paris flows: different ICMP id / sport; merge addresses & flag divergence.
    for flow_i in 1..flow_count {
        let mut flow_cfg = cfg.clone();
        flow_cfg.icmp_id = cfg.icmp_id.wrapping_add(flow_i as u16 * 0x0100);
        flow_cfg.sport = cfg.sport.wrapping_add(flow_i as u16 * 17);
        flow_cfg.enrich = false; // enrich once at end
        let (alt_hops, alt_reached) =
            walk_path(resolved, &flow_cfg, raw_icmp_ok, tcp_ttl_ok, &None).await;
        reached = reached || alt_reached;
        merge_flow_hops(&mut hops, &alt_hops, &mut divergent_ttls);
    }

    if hops.is_empty() {
        warn!("no hop responses received");
    }

    let min_ttl_tcp_reach = if reached {
        hops.last().map(|h| h.ttl)
    } else {
        let mut fast = cfg.clone();
        fast.timeout = Duration::from_millis(400);
        raw::min_ttl_tcp_connect(resolved, &fast).await
    };

    if min_ttl_tcp_reach.is_some() {
        reached = true;
    }

    if cfg.enrich {
        enrich_hops(&mut hops).await;
    }

    let mut summary = PathSummary::from_hops(&hops, min_ttl_tcp_reach, raw_icmp_ok);
    summary.flow_count = Some(flow_count);
    summary.divergent_ttls = divergent_ttls;

    emit(
        &progress,
        ProgressEvent::TraceFinished {
            hops: hops.len(),
            reached,
        },
    );

    Ok(TraceResult {
        target: target.to_string(),
        resolved,
        hops,
        reached,
        probe_kind: cfg.probe_kind,
        dest_port: Some(cfg.dest_port),
        summary,
    })
}

async fn walk_path(
    resolved: IpAddr,
    cfg: &TraceConfig,
    raw_icmp_ok: bool,
    tcp_ttl_ok: bool,
    progress: &Option<ProgressTx>,
) -> (Vec<HopResult>, bool) {
    let mut hops = Vec::new();
    let mut reached = false;

    for ttl in 1..=cfg.max_ttl {
        let merged = probe_one_ttl(resolved, ttl, cfg, raw_icmp_ok, tcp_ttl_ok).await;
        let hop_reached = merged.address.map(|a| a == resolved).unwrap_or(false);
        if hop_reached {
            reached = true;
        }
        let sample_count = merged.samples.len() as u32;
        emit(
            progress,
            ProgressEvent::hop(ttl, cfg.max_ttl, merged.address, sample_count),
        );
        hops.push(merged.into_hop());
        if hop_reached {
            break;
        }
    }
    (hops, reached)
}

fn merge_flow_hops(primary: &mut [HopResult], alt: &[HopResult], divergent: &mut Vec<u8>) {
    for a in alt {
        if let Some(p) = primary.iter_mut().find(|h| h.ttl == a.ttl) {
            match (p.address, a.address) {
                (Some(pa), Some(aa)) if pa != aa => {
                    if !divergent.contains(&a.ttl) {
                        divergent.push(a.ttl);
                    }
                    // Keep primary address; note alt in protos_seen via hostname tag.
                    if p.hostname.is_none() {
                        p.hostname = Some(format!("also {aa}"));
                    }
                }
                (None, Some(aa)) => {
                    p.address = Some(aa);
                    p.reply_proto = a.reply_proto.or(p.reply_proto);
                    for proto in &a.protos_seen {
                        if !p.protos_seen.contains(proto) {
                            p.protos_seen.push(*proto);
                        }
                    }
                    if p.samples_ms.is_empty() {
                        p.samples_ms = a.samples_ms.clone();
                        p.metrics = a.metrics.clone();
                    }
                }
                _ => {
                    for proto in &a.protos_seen {
                        if !p.protos_seen.contains(proto) {
                            p.protos_seen.push(*proto);
                        }
                    }
                }
            }
        }
    }
}

async fn probe_one_ttl(
    resolved: IpAddr,
    ttl: u8,
    cfg: &TraceConfig,
    raw_icmp_ok: bool,
    tcp_ttl_ok: bool,
) -> HopAccumulator {
    let mut merged = HopAccumulator::new(ttl, cfg.probes_per_hop);

    match cfg.probe_kind {
        ProbeKind::Icmp => {
            merged.merge(icmp::probe_ttl(resolved, ttl, cfg, ReplyProto::Icmp).await);
        }
        ProbeKind::Udp => {
            if raw_icmp_ok {
                merged.merge(raw::probe_ttl_udp(resolved, ttl, cfg).await);
            }
            if merged.address.is_none() {
                merged.merge(icmp::probe_ttl(resolved, ttl, cfg, ReplyProto::Icmp).await);
            }
        }
        ProbeKind::Tcp => {
            if raw_icmp_ok && tcp_ttl_ok {
                merged.merge(raw::probe_ttl_tcp(resolved, ttl, cfg, true).await);
            } else if raw_icmp_ok {
                // TTL ignored on TCP — still listen for ICMP Time Exceeded from SYN.
                merged.merge(raw::probe_ttl_tcp(resolved, ttl, cfg, false).await);
            }
            if merged.address.is_none() {
                merged.merge(icmp::probe_ttl(resolved, ttl, cfg, ReplyProto::Icmp).await);
            }
        }
        ProbeKind::Auto => {
            // Raw ICMP Echo (correct TE matching). UDP then TCP only fill remaining gaps.
            merged.merge(icmp::probe_ttl(resolved, ttl, cfg, ReplyProto::Icmp).await);
            if merged.address.is_none() && raw_icmp_ok {
                let mut fast = cfg.clone();
                fast.probes_per_hop = cfg.probes_per_hop.min(2);
                fast.timeout = cfg.timeout.min(Duration::from_millis(900));
                merged.merge(raw::probe_ttl_udp(resolved, ttl, &fast).await);
                if merged.address.is_none() {
                    merged.merge(raw::probe_ttl_tcp(resolved, ttl, &fast, false).await);
                }
            }
        }
    }

    merged
}

#[derive(Debug)]
struct HopAccumulator {
    ttl: u8,
    sent: u8,
    address: Option<IpAddr>,
    reply_proto: Option<ReplyProto>,
    protos_seen: Vec<ReplyProto>,
    samples: Vec<f64>,
    dest_reached: bool,
}

impl HopAccumulator {
    fn new(ttl: u8, sent: u8) -> Self {
        Self {
            ttl,
            sent,
            address: None,
            reply_proto: None,
            protos_seen: Vec::new(),
            samples: Vec::new(),
            dest_reached: false,
        }
    }

    fn merge(&mut self, sample: HopSample) {
        if sample.dest_reached {
            self.dest_reached = true;
        }
        if !sample.samples_ms.is_empty() {
            if let Some(proto) = sample.proto {
                if !self.protos_seen.contains(&proto) {
                    self.protos_seen.push(proto);
                }
                if self.reply_proto.is_none() {
                    self.reply_proto = Some(proto);
                }
            }
            if self.address.is_none() {
                self.address = sample.address;
            }
            self.samples.extend(sample.samples_ms);
        } else if sample.address.is_some() && self.address.is_none() {
            self.address = sample.address;
            if let Some(proto) = sample.proto {
                self.reply_proto = Some(proto);
                if !self.protos_seen.contains(&proto) {
                    self.protos_seen.push(proto);
                }
            }
        }
    }

    fn into_hop(self) -> HopResult {
        HopResult {
            ttl: self.ttl,
            address: self.address,
            hostname: None,
            asn: None,
            as_name: None,
            reply_proto: self.reply_proto,
            protos_seen: self.protos_seen,
            metrics: LatencySummary::from_samples(&self.samples, self.sent as u32),
            samples_ms: self.samples,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct HopSample {
    pub address: Option<IpAddr>,
    pub samples_ms: Vec<f64>,
    pub proto: Option<ReplyProto>,
    pub dest_reached: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_host() {
        assert_eq!(
            strip_url_host("https://api.example.com/v1"),
            "api.example.com"
        );
        assert_eq!(strip_url_host("8.8.8.8"), "8.8.8.8");
        assert_eq!(strip_url_host("example.com:443"), "example.com");
    }

    #[test]
    fn resolve_literal() {
        assert_eq!(
            resolve_target("1.1.1.1").unwrap(),
            "1.1.1.1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn infer_ports() {
        assert_eq!(infer_dest_port("https://example.com/x"), 443);
        assert_eq!(infer_dest_port("http://example.com"), 80);
        assert_eq!(infer_dest_port("https://example.com:8443/a"), 8443);
    }

    #[test]
    fn summary_gaps() {
        let hops = vec![
            HopResult {
                ttl: 1,
                address: Some("10.0.0.1".parse().unwrap()),
                hostname: None,
                asn: None,
                as_name: None,
                reply_proto: Some(ReplyProto::Icmp),
                protos_seen: vec![ReplyProto::Icmp],
                metrics: LatencySummary::from_samples(&[1.0], 1),
                samples_ms: vec![1.0],
            },
            HopResult {
                ttl: 2,
                address: None,
                hostname: None,
                asn: None,
                as_name: None,
                reply_proto: None,
                protos_seen: vec![],
                metrics: LatencySummary::from_samples(&[], 1),
                samples_ms: vec![],
            },
            HopResult {
                ttl: 3,
                address: None,
                hostname: None,
                asn: None,
                as_name: None,
                reply_proto: None,
                protos_seen: vec![],
                metrics: LatencySummary::from_samples(&[], 1),
                samples_ms: vec![],
            },
            HopResult {
                ttl: 4,
                address: Some("1.1.1.1".parse().unwrap()),
                hostname: None,
                asn: Some(13335),
                as_name: Some("CLOUDFLARE".into()),
                reply_proto: Some(ReplyProto::Tcp),
                protos_seen: vec![ReplyProto::Tcp],
                metrics: LatencySummary::from_samples(&[20.0], 1),
                samples_ms: vec![20.0],
            },
        ];
        let s = PathSummary::from_hops(&hops, Some(4), true);
        assert_eq!(s.replied, 2);
        assert_eq!(s.silent, 2);
        assert_eq!(s.gaps.len(), 1);
        assert_eq!(s.gaps[0].from_ttl, 2);
        assert_eq!(s.gaps[0].to_ttl, 3);
        assert_eq!(s.dest_asn, Some(13335));
    }
}
