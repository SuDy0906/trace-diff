//! JSON schema snapshot stability for run + diff reports.

use chrono::DateTime;
use insta::assert_json_snapshot;
use trace_diff::diff::{diff_runs, DiffThresholds};
use trace_diff::l7::L7Metrics;
use trace_diff::meta::{PrivilegeLevel, ProbeMode, RunMetadata};
use trace_diff::stats::LatencySummary;
use trace_diff::store::StoredRun;
use trace_diff::traceroute::{HopResult, TraceResult};
use trace_diff::tui;

fn sample_l7() -> L7Metrics {
    L7Metrics {
        url: "https://example.com/".into(),
        resolved_ip: Some("93.184.216.34".into()),
        status: Some(200),
        dns_ms: Some(12.5),
        tcp_ms: Some(18.0),
        tls_ms: Some(32.25),
        ttfb_ms: Some(80.0),
        transfer_ms: Some(4.0),
        total_ms: 146.75,
        bytes_read: 1256,
        measured_at: DateTime::UNIX_EPOCH,
    }
}

fn sample_meta() -> RunMetadata {
    RunMetadata {
        tool_version: "0.1.0".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        rustc_host: "x86_64-linux".into(),
        probe_mode: ProbeMode::L7Only,
        privileges: PrivilegeLevel::Unprivileged,
        skip_trace: true,
        skip_http: false,
        timing_basis: "std::time::Instant (monotonic)".into(),
        timing_disclaimer: "test-disclaimer".into(),
    }
}

fn sample_run(id: &str, ttfb: f64) -> StoredRun {
    let mut l7 = sample_l7();
    l7.ttfb_ms = Some(ttfb);
    l7.total_ms = ttfb + 66.75;
    StoredRun {
        id: id.into(),
        target: "https://example.com".into(),
        created_at: DateTime::UNIX_EPOCH,
        resolved_ip: Some("93.184.216.34".into()),
        reached: true,
        trace: Some(TraceResult {
            target: "https://example.com".into(),
            resolved: "93.184.216.34".parse().unwrap(),
            reached: true,
            probe_kind: Default::default(),
            dest_port: Some(443),
            summary: Default::default(),
            hops: vec![HopResult {
                ttl: 1,
                address: Some("10.0.0.1".parse().unwrap()),
                hostname: None,
                asn: None,
                as_name: None,
                reply_proto: Some(trace_diff::traceroute::ReplyProto::Icmp),
                protos_seen: vec![trace_diff::traceroute::ReplyProto::Icmp],
                metrics: LatencySummary::from_samples(&[10.0, 12.0, 11.0], 3),
                samples_ms: vec![10.0, 12.0, 11.0],
            }],
        }),
        l7: Some(l7),
        meta: Some(sample_meta()),
    }
}

#[test]
fn snapshot_run_json_schema() {
    let run = sample_run("00000000-0000-0000-0000-000000000001", 80.0);
    let json: serde_json::Value = serde_json::from_str(&tui::render_json(&run, None).unwrap()).unwrap();
    assert_json_snapshot!("run_report", json);
}

#[test]
fn snapshot_diff_json_schema() {
    let baseline = sample_run("00000000-0000-0000-0000-000000000001", 80.0);
    let current = sample_run("00000000-0000-0000-0000-000000000002", 140.0);
    let diff = diff_runs(
        &baseline,
        &current,
        Some("staging".into()),
        &DiffThresholds::default(),
    );
    let json: serde_json::Value =
        serde_json::from_str(&tui::render_json(&current, Some(&diff)).unwrap()).unwrap();
    assert_json_snapshot!("diff_report", json);
}
