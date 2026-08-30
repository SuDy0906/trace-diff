//! ICMP Echo hop probing via surge-ping.

use super::{HopSample, ReplyProto, TraceConfig};
use crate::error::{Error, Result};
use std::net::{IpAddr, ToSocketAddrs};
use surge_ping::{Client, Config, IcmpPacket, PingIdentifier, PingSequence, ICMP};
use tracing::debug;

pub fn strip_url_host(target: &str) -> &str {
    let t = target
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    t.split('/')
        .next()
        .unwrap_or(t)
        .split(':')
        .next()
        .unwrap_or(t)
}

pub fn resolve_target(target: &str) -> Result<IpAddr> {
    let host = strip_url_host(target);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<_> = format!("{host}:0")
        .to_socket_addrs()
        .map_err(|e| Error::Dns {
            host: host.to_string(),
            source: Box::new(e),
        })?
        .map(|sa| sa.ip())
        .collect();
    addrs.into_iter().next().ok_or_else(|| Error::Dns {
        host: host.to_string(),
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no addresses returned",
        )),
    })
}

fn icmp_kind(addr: IpAddr) -> ICMP {
    match addr {
        IpAddr::V4(_) => ICMP::V4,
        IpAddr::V6(_) => ICMP::V6,
    }
}

fn is_destination(resolved: IpAddr, packet: &IcmpPacket) -> bool {
    match (resolved, packet) {
        (IpAddr::V4(dst), IcmpPacket::V4(pkt)) => pkt.get_source() == dst,
        (IpAddr::V6(dst), IcmpPacket::V6(pkt)) => pkt.get_source() == dst,
        _ => false,
    }
}

fn packet_source(packet: &IcmpPacket) -> IpAddr {
    match packet {
        IcmpPacket::V4(pkt) => IpAddr::V4(pkt.get_source()),
        IcmpPacket::V6(pkt) => IpAddr::V6(pkt.get_source()),
    }
}

pub async fn probe_ttl(
    resolved: IpAddr,
    ttl: u8,
    cfg: &TraceConfig,
    proto: ReplyProto,
) -> HopSample {
    // Prefer raw ICMP Echo matching (sees Time Exceeded from routers).
    // surge-ping keys waiters by dest IP, so intermediate TE replies never land.
    if crate::traceroute::raw::raw_icmp_available(resolved) {
        let mut sample = crate::traceroute::raw::probe_ttl_icmp(resolved, ttl, cfg).await;
        if sample.proto.is_none() && sample.address.is_some() {
            sample.proto = Some(proto);
        }
        return sample;
    }

    probe_ttl_surge(resolved, ttl, cfg, proto).await
}

async fn probe_ttl_surge(
    resolved: IpAddr,
    ttl: u8,
    cfg: &TraceConfig,
    proto: ReplyProto,
) -> HopSample {
    let kind = icmp_kind(resolved);
    let config = Config::builder().kind(kind).ttl(ttl as u32).build();
    let client = match Client::new(&config) {
        Ok(c) => c,
        Err(e) => {
            debug!(ttl, error = %e, "icmp client open failed");
            return HopSample::default();
        }
    };

    let ident = PingIdentifier(cfg.icmp_id);
    let mut samples = Vec::new();
    let mut last_addr = None;
    let mut dest_reached = false;

    for probe_idx in 0..cfg.probes_per_hop {
        let seq = PingSequence(((ttl as u16) << 8) | probe_idx as u16);
        let mut pinger = client.pinger(resolved, ident).await;
        pinger.timeout(cfg.timeout);

        match pinger.ping(seq, &[ttl, probe_idx]).await {
            Ok((packet, rtt)) => {
                let src = packet_source(&packet);
                last_addr = Some(src);
                samples.push(rtt.as_secs_f64() * 1000.0);
                if is_destination(resolved, &packet) {
                    dest_reached = true;
                }
            }
            Err(e) => {
                debug!(ttl, probe = probe_idx, error = %e, "icmp probe timeout/error");
            }
        }
    }

    HopSample {
        address: last_addr,
        samples_ms: samples,
        proto: if last_addr.is_some() {
            Some(proto)
        } else {
            None
        },
        dest_reached,
    }
}
