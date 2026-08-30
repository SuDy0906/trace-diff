//! L7 HTTP connection lifecycle prober.
//!
//! Instruments DNS → TCP handshake → TLS → TTFB → transfer with
//! `std::time::Instant` monotonic clocks.

use crate::error::{Error, Result};
use crate::progress::ProgressEvent;
use crate::traceroute::ProgressTx;
use chrono::{DateTime, Utc};
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::TokioAsyncResolver;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::debug;
use url::Url;

fn emit(tx: &Option<ProgressTx>, phase: &str) {
    if let Some(tx) = tx {
        let _ = tx.send(ProgressEvent::L7Phase {
            phase: phase.to_string(),
        });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L7Metrics {
    pub url: String,
    pub resolved_ip: Option<String>,
    pub status: Option<u16>,
    /// DNS resolution duration (T0→T1)
    pub dns_ms: Option<f64>,
    /// TCP handshake duration (T1→T2)
    pub tcp_ms: Option<f64>,
    /// TLS handshake duration (T2→T3)
    pub tls_ms: Option<f64>,
    /// Time to first byte / server processing (T3→T4)
    pub ttfb_ms: Option<f64>,
    /// Content transfer duration (T4→T5)
    pub transfer_ms: Option<f64>,
    /// End-to-end wall time
    pub total_ms: f64,
    pub bytes_read: u64,
    pub measured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertCanary {
    pub host: String,
    pub tls_version: String,
    pub handshake_ms: f64,
    pub days_until_expiry: Option<i64>,
    pub not_after: Option<String>,
    pub issuer: Option<String>,
    pub subject: Option<String>,
}

#[derive(Debug, Clone)]
pub struct L7Config {
    pub timeout: Duration,
    pub method: String,
    pub max_body_bytes: usize,
    /// Extra request headers (e.g. Authorization).
    pub extra_headers: Vec<(String, String)>,
}

impl Default for L7Config {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            method: "GET".into(),
            max_body_bytes: 256 * 1024,
            extra_headers: Vec::new(),
        }
    }
}

/// Probe the full HTTP(S) connection lifecycle for `target`.
pub async fn probe(target: &str, cfg: L7Config) -> Result<L7Metrics> {
    probe_with_progress(target, cfg, None).await
}

/// Same as [`probe`], optionally streaming phase progress.
pub async fn probe_with_progress(
    target: &str,
    cfg: L7Config,
    progress: Option<ProgressTx>,
) -> Result<L7Metrics> {
    let url_str = normalize_url(target)?;
    let url = Url::parse(&url_str).map_err(|e| Error::InvalidTarget(e.to_string()))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidTarget("missing host".into()))?
        .to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let use_tls = url.scheme() == "https";
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let request_target = format!("{path}{query}");

    let t0 = Instant::now();

    // T0 → T1: DNS
    emit(&progress, "DNS resolution");
    let dns_start = Instant::now();
    let addr = resolve_dns(&host, port).await?;
    let dns_ms = elapsed_ms(dns_start);
    debug!(%addr, dns_ms, "DNS resolved");

    // T1 → T2: TCP
    emit(&progress, "TCP handshake");
    let tcp_start = Instant::now();
    let tcp = tokio::time::timeout(cfg.timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::HttpProbe("TCP connect timed out".into()))?
        .map_err(|e| Error::HttpProbe(format!("TCP connect failed: {e}")))?;
    let tcp_ms = elapsed_ms(tcp_start);
    debug!(tcp_ms, "TCP handshake complete");

    let (mut reader, mut writer): (
        Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    );

    let tls_ms = if use_tls {
        // T2 → T3: TLS
        emit(&progress, "TLS handshake");
        let tls_start = Instant::now();
        let connector = build_tls_connector()?;
        let server_name = ServerName::try_from(host.clone())
            .map_err(|e| Error::HttpProbe(format!("invalid SNI: {e}")))?;
        let tls = tokio::time::timeout(cfg.timeout, connector.connect(server_name, tcp))
            .await
            .map_err(|_| Error::HttpProbe("TLS handshake timed out".into()))?
            .map_err(|e| Error::HttpProbe(format!("TLS handshake failed: {e}")))?;
        let ms = elapsed_ms(tls_start);
        debug!(tls_ms = ms, "TLS handshake complete");
        let (r, w) = tokio::io::split(tls);
        reader = Box::new(r);
        writer = Box::new(w);
        Some(ms)
    } else {
        let (r, w) = tokio::io::split(tcp);
        reader = Box::new(r);
        writer = Box::new(w);
        None
    };

    // Send HTTP/1.1 request
    emit(&progress, "request / TTFB");
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: trace-diff/{}\r\nConnection: close\r\nAccept: */*\r\n",
        cfg.method, request_target, host, crate::meta::VERSION
    );
    for (name, value) in &cfg.extra_headers {
        req.push_str(&format!("{name}: {value}\r\n"));
    }
    req.push_str("\r\n");
    writer
        .write_all(req.as_bytes())
        .await
        .map_err(|e| Error::HttpProbe(format!("write request failed: {e}")))?;
    writer
        .flush()
        .await
        .map_err(|e| Error::HttpProbe(format!("flush failed: {e}")))?;

    // T3 → T4: TTFB
    let ttfb_start = Instant::now();
    let mut buf = vec![0u8; 8 * 1024];
    let n = tokio::time::timeout(cfg.timeout, reader.read(&mut buf))
        .await
        .map_err(|_| Error::HttpProbe("TTFB timed out".into()))?
        .map_err(|e| Error::HttpProbe(format!("read failed: {e}")))?;
    if n == 0 {
        return Err(Error::HttpProbe("empty response".into()));
    }
    let ttfb_ms = elapsed_ms(ttfb_start);
    debug!(ttfb_ms, first_bytes = n, "TTFB");

    let status = parse_status(&buf[..n]);

    // T4 → T5: transfer remainder
    emit(&progress, "content transfer");
    let transfer_start = Instant::now();
    let mut bytes_read = n as u64;
    loop {
        if bytes_read as usize >= cfg.max_body_bytes {
            break;
        }
        match tokio::time::timeout(cfg.timeout, reader.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(m)) => bytes_read += m as u64,
            Ok(Err(e)) => return Err(Error::HttpProbe(format!("body read failed: {e}"))),
            Err(_) => break,
        }
    }
    let transfer_ms = elapsed_ms(transfer_start);
    let total_ms = elapsed_ms(t0);

    if let Some(tx) = &progress {
        let _ = tx.send(ProgressEvent::L7Finished { total_ms, status });
    }

    Ok(L7Metrics {
        url: url_str,
        resolved_ip: Some(addr.ip().to_string()),
        status,
        dns_ms: Some(dns_ms),
        tcp_ms: Some(tcp_ms),
        tls_ms,
        ttfb_ms: Some(ttfb_ms),
        transfer_ms: Some(transfer_ms),
        total_ms,
        bytes_read,
        measured_at: Utc::now(),
    })
}

fn normalize_url(target: &str) -> Result<String> {
    let t = target.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        Ok(t.to_string())
    } else if t.parse::<std::net::IpAddr>().is_ok() || !t.contains("://") {
        // Bare host → assume HTTPS
        Ok(format!("https://{t}"))
    } else {
        Err(Error::InvalidTarget(t.to_string()))
    }
}

async fn resolve_dns(host: &str, port: u16) -> Result<SocketAddr> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default());
    let response = resolver.lookup_ip(host).await.map_err(|e| Error::Dns {
        host: host.to_string(),
        source: Box::new(e),
    })?;
    let ip = response.iter().next().ok_or_else(|| Error::Dns {
        host: host.to_string(),
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no A/AAAA records",
        )),
    })?;
    Ok(SocketAddr::new(ip, port))
}

fn build_tls_connector() -> Result<TlsConnector> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// TLS handshake + certificate expiry canary for an HTTPS host.
pub async fn probe_cert(target: &str, timeout: Duration) -> Result<CertCanary> {
    let url = if target.starts_with("http://") || target.starts_with("https://") {
        Url::parse(target).map_err(|e| Error::InvalidTarget(e.to_string()))?
    } else {
        Url::parse(&format!("https://{target}")).map_err(|e| Error::InvalidTarget(e.to_string()))?
    };
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidTarget("missing host".into()))?
        .to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addr = resolve_dns(&host, port).await?;
    let tcp = tokio::time::timeout(timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| Error::HttpProbe("TCP connect timed out".into()))?
        .map_err(|e| Error::HttpProbe(format!("TCP connect failed: {e}")))?;
    let connector = build_tls_connector()?;
    let server_name = ServerName::try_from(host.clone())
        .map_err(|e| Error::HttpProbe(format!("invalid SNI: {e}")))?;
    let start = Instant::now();
    let tls = tokio::time::timeout(timeout, connector.connect(server_name, tcp))
        .await
        .map_err(|_| Error::HttpProbe("TLS handshake timed out".into()))?
        .map_err(|e| Error::HttpProbe(format!("TLS handshake failed: {e}")))?;
    let handshake_ms = elapsed_ms(start);

    let conn = tls.get_ref().1;
    let tls_version = conn
        .protocol_version()
        .map(|v| format!("{v:?}"))
        .unwrap_or_else(|| "unknown".into());

    let mut days_until_expiry = None;
    let mut not_after = None;
    let mut issuer = None;
    let mut subject = None;
    if let Some(certs) = conn.peer_certificates() {
        if let Some(der) = certs.first() {
            if let Ok((_, x509)) = x509_parser::parse_x509_certificate(der.as_ref()) {
                subject = Some(x509.subject().to_string());
                issuer = Some(x509.issuer().to_string());
                let na = x509.validity().not_after;
                not_after = Some(na.to_string());
                let unix = na.timestamp();
                let now = chrono::Utc::now().timestamp();
                days_until_expiry = Some((unix - now) / 86_400);
            }
        }
    }

    Ok(CertCanary {
        host,
        tls_version,
        handshake_ms,
        days_until_expiry,
        not_after,
        issuer,
        subject,
    })
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn parse_status(bytes: &[u8]) -> Option<u16> {
    let s = std::str::from_utf8(bytes).ok()?;
    let line = s.lines().next()?;
    let mut parts = line.split_whitespace();
    let _http = parts.next()?;
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        assert_eq!(normalize_url("example.com").unwrap(), "https://example.com");
        assert_eq!(
            normalize_url("http://example.com/x").unwrap(),
            "http://example.com/x"
        );
    }

    #[test]
    fn status_parse() {
        assert_eq!(parse_status(b"HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_status(b"HTTP/1.0 404 Not Found\r\n"), Some(404));
    }
}
