//! Reverse DNS + Team Cymru ASN enrichment for hop IPs.

use super::HopResult;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::proto::rr::RecordType;
use hickory_resolver::TokioAsyncResolver;
use std::net::IpAddr;
use tracing::debug;

/// Fill hostname / ASN / AS name for hops that have an address.
pub async fn enrich_hops(hops: &mut [HopResult]) {
    let resolver = TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default());

    for hop in hops.iter_mut() {
        let Some(ip) = hop.address else {
            continue;
        };
        if hop.hostname.is_none() {
            hop.hostname = reverse_dns(&resolver, ip).await;
        }
        if hop.asn.is_none() {
            if let Some((asn, name)) = cymru_asn(&resolver, ip).await {
                hop.asn = Some(asn);
                if hop.as_name.is_none() {
                    hop.as_name = name;
                }
            }
        }
    }
}

async fn reverse_dns(resolver: &TokioAsyncResolver, ip: IpAddr) -> Option<String> {
    match resolver.reverse_lookup(ip).await {
        Ok(lookup) => lookup.iter().next().map(|n| {
            let s = n.to_string();
            s.trim_end_matches('.').to_string()
        }),
        Err(e) => {
            debug!(%ip, error = %e, "PTR lookup failed");
            None
        }
    }
}

async fn cymru_asn(resolver: &TokioAsyncResolver, ip: IpAddr) -> Option<(u32, Option<String>)> {
    let IpAddr::V4(v4) = ip else {
        return None;
    };
    let octets = v4.octets();
    let qname = format!(
        "{}.{}.{}.{}.origin.asn.cymru.com.",
        octets[3], octets[2], octets[1], octets[0]
    );

    let lookup = resolver.lookup(qname, RecordType::TXT).await.ok()?;
    let txt = lookup.iter().next()?.to_string();
    let asn: u32 = txt.split('|').next()?.trim().parse().ok()?;

    let as_name = as_name_lookup(resolver, asn).await;
    Some((asn, as_name))
}

async fn as_name_lookup(resolver: &TokioAsyncResolver, asn: u32) -> Option<String> {
    let qname = format!("AS{asn}.asn.cymru.com.");
    let lookup = resolver.lookup(qname, RecordType::TXT).await.ok()?;
    let txt = lookup.iter().next()?.to_string();
    let name = txt.split('|').nth(4)?.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}
