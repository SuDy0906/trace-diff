//! UDP / TCP / ICMP hop probes with proper ICMP Time Exceeded matching.
//!
//! `surge-ping` keys waiters by destination IP, so Time Exceeded from intermediate
//! routers never completes a ping — hops look silent. This module sends probes and
//! matches replies by embedded id/seq (ICMP) or ports (UDP/TCP).

use super::{HopSample, ReplyProto, TraceConfig};
use pnet_packet::icmp::echo_reply::EchoReplyPacket;
use pnet_packet::icmp::echo_request::MutableEchoRequestPacket;
use pnet_packet::icmp::{checksum as icmp_checksum, IcmpPacket, IcmpTypes};
use pnet_packet::icmpv6::{Icmpv6Packet, Icmpv6Types};
use pnet_packet::ip::IpNextHeaderProtocols;
use pnet_packet::ipv4::Ipv4Packet;
use pnet_packet::ipv6::Ipv6Packet;
use pnet_packet::tcp::TcpPacket;
use pnet_packet::udp::UdpPacket;
use pnet_packet::Packet;
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::mem::MaybeUninit;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::time::{Duration, Instant};
use tracing::debug;

pub fn raw_icmp_available(dst: IpAddr) -> bool {
    #[cfg(windows)]
    {
        if matches!(dst, IpAddr::V4(_)) {
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::NetworkManagement::IpHelper::{
                IcmpCloseHandle, IcmpCreateFile,
            };
            let handle: HANDLE = unsafe { IcmpCreateFile() };
            if !handle.is_null() && handle != (-1isize as HANDLE) {
                unsafe {
                    let _ = IcmpCloseHandle(handle);
                }
                return true;
            }
        }
    }
    open_icmp_socket(dst).is_ok()
}

fn open_icmp_socket(dst: IpAddr) -> io::Result<Socket> {
    match dst {
        IpAddr::V4(_) => {
            let s = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4))?;
            s.set_read_timeout(Some(Duration::from_millis(150)))?;
            Ok(s)
        }
        IpAddr::V6(_) => {
            let s = Socket::new(Domain::IPV6, Type::RAW, Some(Protocol::ICMPV6))?;
            s.set_read_timeout(Some(Duration::from_millis(150)))?;
            Ok(s)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IcmpHint {
    TimeExceeded,
    DestUnreach,
    EchoReply,
    Other,
}

struct IcmpSighting {
    from: IpAddr,
    hint: IcmpHint,
    /// ICMP Echo id/seq or UDP/TCP (sport, dport) from this packet or its embed.
    key: Option<(u16, u16)>,
    /// Original datagram destination from ICMP error embed (best hop matcher).
    embedded_dst: Option<IpAddr>,
}

fn parse_ipv4_icmp(buf: &[u8]) -> Option<IcmpSighting> {
    let (offset, outer_src) = if let Some(ip) = Ipv4Packet::new(buf) {
        if ip.get_next_level_protocol() != IpNextHeaderProtocols::Icmp {
            return None;
        }
        let hdr = ip.get_header_length() as usize * 4;
        (hdr, IpAddr::V4(ip.get_source()))
    } else {
        (0, IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    };
    if offset >= buf.len() {
        return None;
    }
    let icmp = IcmpPacket::new(&buf[offset..])?;
    let hint = match icmp.get_icmp_type() {
        t if t == IcmpTypes::TimeExceeded => IcmpHint::TimeExceeded,
        t if t == IcmpTypes::DestinationUnreachable => IcmpHint::DestUnreach,
        t if t == IcmpTypes::EchoReply => IcmpHint::EchoReply,
        _ => IcmpHint::Other,
    };

    let (key, embedded_dst) = match hint {
        IcmpHint::EchoReply => {
            let echo = EchoReplyPacket::new(icmp.packet())?;
            (
                Some((echo.get_identifier(), echo.get_sequence_number())),
                None,
            )
        }
        IcmpHint::TimeExceeded | IcmpHint::DestUnreach => {
            let (k, d) = embed_info_v4(icmp.payload());
            (k, d)
        }
        IcmpHint::Other => (None, None),
    };

    Some(IcmpSighting {
        from: outer_src,
        hint,
        key,
        embedded_dst,
    })
}

fn embed_info_v4(payload: &[u8]) -> (Option<(u16, u16)>, Option<IpAddr>) {
    let ip_bytes = if Ipv4Packet::new(payload).is_some() {
        payload
    } else if payload.len() > 4 && Ipv4Packet::new(&payload[4..]).is_some() {
        &payload[4..]
    } else {
        return (None, None);
    };
    let Some(ip) = Ipv4Packet::new(ip_bytes) else {
        return (None, None);
    };
    let dst = IpAddr::V4(ip.get_destination());
    let transport = ip.payload();
    let key = match ip.get_next_level_protocol() {
        IpNextHeaderProtocols::Udp => {
            UdpPacket::new(transport).map(|u| (u.get_source(), u.get_destination()))
        }
        IpNextHeaderProtocols::Tcp => {
            TcpPacket::new(transport).map(|t| (t.get_source(), t.get_destination()))
        }
        IpNextHeaderProtocols::Icmp if transport.len() >= 8 => Some((
            u16::from_be_bytes([transport[4], transport[5]]),
            u16::from_be_bytes([transport[6], transport[7]]),
        )),
        _ => None,
    };
    (key, Some(dst))
}

fn parse_ipv6_icmp(buf: &[u8]) -> Option<IcmpSighting> {
    let (offset, outer_src) = if let Some(ip) = Ipv6Packet::new(buf) {
        if ip.get_next_header() != IpNextHeaderProtocols::Icmpv6 {
            return None;
        }
        (40usize, IpAddr::V6(ip.get_source()))
    } else {
        (0, IpAddr::V6(Ipv6Addr::UNSPECIFIED))
    };
    if offset >= buf.len() {
        return None;
    }
    let icmp = Icmpv6Packet::new(&buf[offset..])?;
    let hint = match icmp.get_icmpv6_type() {
        t if t == Icmpv6Types::TimeExceeded => IcmpHint::TimeExceeded,
        t if t == Icmpv6Types::DestinationUnreachable => IcmpHint::DestUnreach,
        t if t == Icmpv6Types::EchoReply => IcmpHint::EchoReply,
        _ => IcmpHint::Other,
    };
    Some(IcmpSighting {
        from: outer_src,
        hint,
        key: None,
        embedded_dst: None,
    })
}

fn is_our_reply(sighting: &IcmpSighting, expect: (u16, u16), dst: IpAddr, loose: bool) -> bool {
    if keys_match(sighting.key, expect, loose) {
        return true;
    }
    // Classic traceroute fallback: any TE/Unreach whose embedded dest is our target.
    matches!(
        sighting.hint,
        IcmpHint::TimeExceeded | IcmpHint::DestUnreach
    ) && sighting.embedded_dst == Some(dst)
}

fn keys_match(got: Option<(u16, u16)>, expect: (u16, u16), loose_sport_only: bool) -> bool {
    match got {
        Some((a, b)) if a == expect.0 && b == expect.1 => true,
        Some((a, _)) if loose_sport_only && a == expect.0 => true,
        _ => false,
    }
}

fn recv_matched_icmp(
    sock: &Socket,
    timeout: Duration,
    expect: (u16, u16),
    dst: IpAddr,
    loose: bool,
) -> Option<(IpAddr, bool)> {
    let deadline = Instant::now() + timeout;
    let mut buf = [MaybeUninit::<u8>::uninit(); 2048];
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));

    while Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => {
                let peer_ip = addr.as_socket().map(|s| s.ip());
                let slice = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, n) };
                let mut sighting = match dst {
                    IpAddr::V4(_) => match parse_ipv4_icmp(slice) {
                        Some(s) => s,
                        None => continue,
                    },
                    IpAddr::V6(_) => match parse_ipv6_icmp(slice) {
                        Some(s) => s,
                        None => continue,
                    },
                };
                if matches!(sighting.from, IpAddr::V4(v) if v.is_unspecified())
                    || matches!(sighting.from, IpAddr::V6(v) if v.is_unspecified())
                {
                    if let Some(p) = peer_ip {
                        sighting.from = p;
                    }
                }
                if !is_our_reply(&sighting, expect, dst, loose) {
                    continue;
                }
                match sighting.hint {
                    IcmpHint::TimeExceeded => return Some((sighting.from, false)),
                    IcmpHint::DestUnreach => {
                        return Some((sighting.from, sighting.from == dst));
                    }
                    IcmpHint::EchoReply => {
                        return Some((sighting.from, sighting.from == dst));
                    }
                    IcmpHint::Other => continue,
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(_) => break,
        }
    }
    None
}

/// ICMP Echo traceroute that correctly attributes Time Exceeded to intermediate hops.
pub async fn probe_ttl_icmp(resolved: IpAddr, ttl: u8, cfg: &TraceConfig) -> HopSample {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            if let IpAddr::V4(v4) = resolved {
                return probe_ttl_icmp_windows(v4, ttl, &cfg);
            }
        }
        probe_ttl_icmp_raw(resolved, ttl, cfg)
    })
    .await
    .unwrap_or_default()
}

/// Windows ICMP Helper API (same path as `tracert`) — raw sockets miss Time Exceeded here.
#[cfg(windows)]
fn probe_ttl_icmp_windows(dest: Ipv4Addr, ttl: u8, cfg: &TraceConfig) -> HopSample {
    use std::ptr;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY, IP_OPTION_INFORMATION,
        IP_SUCCESS,
    };

    // IP_TTL_EXPIRED_TRANSIT
    const IP_TTL_EXPIRED_TRANSIT: u32 = 11013;

    let handle: HANDLE = unsafe { IcmpCreateFile() };
    if handle.is_null() || handle == (-1isize as HANDLE) {
        debug!("IcmpCreateFile failed; falling back to raw ICMP");
        return probe_ttl_icmp_raw(IpAddr::V4(dest), ttl, cfg.clone());
    }

    let dest_u32 = u32::from_ne_bytes(dest.octets());
    let payload = b"trace-diff";
    let mut samples = Vec::new();
    let mut last_addr = None;
    let mut dest_reached = false;
    let timeout_ms = cfg.timeout.as_millis().min(u32::MAX as u128) as u32;

    for _ in 0..cfg.probes_per_hop {
        let options = IP_OPTION_INFORMATION {
            Ttl: ttl,
            Tos: 0,
            Flags: 0,
            OptionsSize: 0,
            OptionsData: ptr::null_mut(),
        };
        let reply_size = std::mem::size_of::<ICMP_ECHO_REPLY>() + payload.len() + 16;
        let mut reply = vec![0u8; reply_size];
        let start = Instant::now();
        let n = unsafe {
            IcmpSendEcho(
                handle,
                dest_u32,
                payload.as_ptr() as *const _,
                payload.len() as u16,
                &options,
                reply.as_mut_ptr() as *mut _,
                reply_size as u32,
                timeout_ms,
            )
        };
        if n == 0 {
            continue;
        }
        let echo = unsafe { &*(reply.as_ptr() as *const ICMP_ECHO_REPLY) };
        let from = Ipv4Addr::from(u32::to_ne_bytes(echo.Address));
        let rtt = if echo.RoundTripTime > 0 {
            echo.RoundTripTime as f64
        } else {
            start.elapsed().as_secs_f64() * 1000.0
        };
        match echo.Status {
            IP_SUCCESS => {
                samples.push(rtt);
                last_addr = Some(IpAddr::V4(from));
                if from == dest {
                    dest_reached = true;
                }
            }
            IP_TTL_EXPIRED_TRANSIT => {
                samples.push(rtt);
                last_addr = Some(IpAddr::V4(from));
            }
            other => {
                debug!(status = other, %from, "icmp helper status");
                // Some stacks still populate Address on other TTL-related codes.
                if from != Ipv4Addr::UNSPECIFIED && from != dest {
                    samples.push(rtt);
                    last_addr = Some(IpAddr::V4(from));
                }
            }
        }
    }

    unsafe {
        let _ = IcmpCloseHandle(handle);
    }

    HopSample {
        address: last_addr,
        samples_ms: samples,
        proto: last_addr.map(|_| ReplyProto::Icmp),
        dest_reached,
    }
}

fn probe_ttl_icmp_raw(resolved: IpAddr, ttl: u8, cfg: TraceConfig) -> HopSample {
    let Ok(sock) = open_icmp_socket(resolved) else {
        return HopSample::default();
    };
    if let Err(e) = set_ttl(&sock, resolved, ttl) {
        debug!(error = %e, "icmp set_ttl failed");
        return HopSample::default();
    }

    let ident = cfg.icmp_id;
    let mut samples = Vec::new();
    let mut last_addr = None;
    let mut dest_reached = false;
    let dest = socket_addr(resolved, 0);

    for probe_idx in 0..cfg.probes_per_hop {
        let seq = ((ttl as u16) << 8) | probe_idx as u16;
        let packet = match build_echo_request(ident, seq) {
            Some(p) => p,
            None => continue,
        };
        let start = Instant::now();
        if sock.send_to(&packet, &dest.into()).is_err() {
            continue;
        }
        if let Some((from, reached)) =
            recv_matched_icmp(&sock, cfg.timeout, (ident, seq), resolved, false)
        {
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            last_addr = Some(from);
            if reached || from == resolved {
                dest_reached = true;
            }
        }
    }

    HopSample {
        address: last_addr,
        samples_ms: samples,
        proto: last_addr.map(|_| ReplyProto::Icmp),
        dest_reached,
    }
}

fn build_echo_request(ident: u16, seq: u16) -> Option<Vec<u8>> {
    let payload = b"trace-diff";
    let mut buf = vec![0u8; 8 + payload.len()];
    {
        let mut echo = MutableEchoRequestPacket::new(&mut buf)?;
        echo.set_icmp_type(IcmpTypes::EchoRequest);
        echo.set_identifier(ident);
        echo.set_sequence_number(seq);
        echo.set_payload(payload);
    }
    let check = {
        let pkt = IcmpPacket::new(&buf)?;
        icmp_checksum(&pkt)
    };
    let mut echo = MutableEchoRequestPacket::new(&mut buf)?;
    echo.set_checksum(check);
    Some(buf)
}

pub async fn probe_ttl_udp(resolved: IpAddr, ttl: u8, cfg: &TraceConfig) -> HopSample {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || probe_ttl_udp_sync(resolved, ttl, cfg))
        .await
        .unwrap_or_default()
}

fn probe_ttl_udp_sync(resolved: IpAddr, ttl: u8, cfg: TraceConfig) -> HopSample {
    let Ok(listener) = open_icmp_socket(resolved) else {
        return HopSample::default();
    };

    let domain = match resolved {
        IpAddr::V4(_) => Domain::IPV4,
        IpAddr::V6(_) => Domain::IPV6,
    };
    let Ok(sock) = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP)) else {
        return HopSample::default();
    };

    let sport = cfg.sport;
    let dport = 33_434u16.saturating_add(ttl as u16);
    if let Err(e) = bind_sport(&sock, resolved, sport) {
        debug!(error = %e, "udp bind failed");
        return HopSample::default();
    }
    if let Err(e) = set_ttl(&sock, resolved, ttl) {
        debug!(error = %e, "udp set_ttl failed");
        return HopSample::default();
    }

    let dest = socket_addr(resolved, dport);
    let mut samples = Vec::new();
    let mut last_addr = None;
    let mut dest_reached = false;

    for _ in 0..cfg.probes_per_hop {
        let start = Instant::now();
        let _ = sock.send_to(b"td", &dest.into());
        // Loose match: some stacks truncate the embedded UDP header.
        if let Some((from, reached)) =
            recv_matched_icmp(&listener, cfg.timeout, (sport, dport), resolved, true)
        {
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            last_addr = Some(from);
            if reached || from == resolved {
                dest_reached = true;
            }
        }
    }

    HopSample {
        address: last_addr,
        samples_ms: samples,
        proto: last_addr.map(|_| ReplyProto::Udp),
        dest_reached,
    }
}

pub async fn probe_ttl_tcp(
    resolved: IpAddr,
    ttl: u8,
    cfg: &TraceConfig,
    allow_connect_as_dest: bool,
) -> HopSample {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        probe_ttl_tcp_sync(resolved, ttl, cfg, allow_connect_as_dest)
    })
    .await
    .unwrap_or_default()
}

fn probe_ttl_tcp_sync(
    resolved: IpAddr,
    ttl: u8,
    cfg: TraceConfig,
    allow_connect_as_dest: bool,
) -> HopSample {
    let listener = open_icmp_socket(resolved).ok();
    let domain = match resolved {
        IpAddr::V4(_) => Domain::IPV4,
        IpAddr::V6(_) => Domain::IPV6,
    };

    let mut samples = Vec::new();
    let mut last_addr = None;
    let mut dest_reached = false;
    let sport = cfg.sport.wrapping_add(1);
    let dest = socket_addr(resolved, cfg.dest_port);

    for idx in 0..cfg.probes_per_hop {
        let Ok(sock) = Socket::new(domain, Type::STREAM, Some(Protocol::TCP)) else {
            break;
        };
        let _ = sock.set_nonblocking(true);
        let _ = set_ttl(&sock, resolved, ttl);
        let probe_sport = sport.wrapping_add(idx as u16);
        let _ = bind_sport(&sock, resolved, probe_sport);

        let start = Instant::now();
        let _ = sock.connect(&dest.into());

        if let Some(ref listener) = listener {
            if let Some((from, reached)) = recv_matched_icmp(
                listener,
                cfg.timeout,
                (probe_sport, cfg.dest_port),
                resolved,
                true,
            ) {
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
                last_addr = Some(from);
                if reached || from == resolved {
                    dest_reached = true;
                }
                continue;
            }
        }

        if allow_connect_as_dest {
            let deadline = start + cfg.timeout;
            while Instant::now() < deadline {
                if sock.peer_addr().is_ok() {
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    last_addr = Some(resolved);
                    dest_reached = true;
                    break;
                }
                if let Ok(Some(err)) = sock.take_error() {
                    if err.kind() == io::ErrorKind::ConnectionRefused {
                        samples.push(start.elapsed().as_secs_f64() * 1000.0);
                        last_addr = Some(resolved);
                        dest_reached = true;
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    HopSample {
        address: last_addr,
        samples_ms: samples,
        proto: last_addr.map(|_| ReplyProto::Tcp),
        dest_reached,
    }
}

pub async fn tcp_ttl_appears_honored(resolved: IpAddr, cfg: &TraceConfig) -> bool {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        let mut fast = cfg;
        fast.timeout = Duration::from_millis(400);
        !tcp_connect_with_ttl(resolved, 1, &fast)
    })
    .await
    .unwrap_or(false)
}

pub async fn min_ttl_tcp_connect(resolved: IpAddr, cfg: &TraceConfig) -> Option<u8> {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || {
        (1..=cfg.max_ttl.min(32)).find(|&ttl| tcp_connect_with_ttl(resolved, ttl, &cfg))
    })
    .await
    .ok()
    .flatten()
}

fn tcp_connect_with_ttl(resolved: IpAddr, ttl: u8, cfg: &TraceConfig) -> bool {
    let domain = match resolved {
        IpAddr::V4(_) => Domain::IPV4,
        IpAddr::V6(_) => Domain::IPV6,
    };
    let Ok(sock) = Socket::new(domain, Type::STREAM, Some(Protocol::TCP)) else {
        return false;
    };
    let _ = set_ttl(&sock, resolved, ttl);
    let _ = sock.set_read_timeout(Some(cfg.timeout));
    let _ = sock.set_write_timeout(Some(cfg.timeout));
    let dest = socket_addr(resolved, cfg.dest_port);
    match sock.connect(&dest.into()) {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::ConnectionRefused,
    }
}

fn bind_sport(sock: &Socket, resolved: IpAddr, sport: u16) -> io::Result<()> {
    let addr = socket_addr(
        match resolved {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        },
        sport,
    );
    sock.bind(&addr.into())
}

fn set_ttl(sock: &Socket, resolved: IpAddr, ttl: u8) -> io::Result<()> {
    match resolved {
        IpAddr::V4(_) => sock.set_ttl(ttl as u32),
        IpAddr::V6(_) => sock.set_unicast_hops_v6(ttl as u32),
    }
}

fn socket_addr(ip: IpAddr, port: u16) -> SocketAddr {
    match ip {
        IpAddr::V4(v4) => SocketAddr::V4(SocketAddrV4::new(v4, port)),
        IpAddr::V6(v6) => SocketAddr::V6(SocketAddrV6::new(v6, port, 0, 0)),
    }
}
