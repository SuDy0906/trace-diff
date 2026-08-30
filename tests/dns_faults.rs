//! DNS fault cases against the L7 prober.

use trace_diff::error::Error;
use trace_diff::l7::{self, L7Config};

#[tokio::test]
async fn nxdomain_host_fails_dns_phase() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let res = l7::probe(
        "https://this-domain-should-not-exist-trace-diff-xyz.invalid/",
        L7Config {
            timeout: std::time::Duration::from_secs(5),
            ..L7Config::default()
        },
    )
    .await;
    match res {
        Err(Error::Dns { .. }) | Err(Error::HttpProbe(_)) | Err(Error::InvalidTarget(_)) => {}
        Err(other) => panic!("unexpected error variant: {other}"),
        Ok(m) => panic!("expected DNS/connect failure, got ok: {m:?}"),
    }
}
