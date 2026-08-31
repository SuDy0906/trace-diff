//! CLI integration tests (wiremock — no live network required).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn help_works() {
    Command::cargo_bin("trace-diff")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("L3/L4 traceroute"));
}

#[test]
fn run_http_headless_saves_baseline() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let dir = tempdir().unwrap();
        let db = dir.path().join("test.db");
        let base = format!("{}/", server.uri());

        Command::cargo_bin("trace-diff")
            .unwrap()
            .args([
                "run",
                &base,
                "--skip-trace",
                "--headless",
                "--save-baseline",
                "ci-baseline",
                "--db",
            ])
            .arg(&db)
            .assert()
            .success()
            .stdout(predicate::str::contains("ttfb_ms"));

        Command::cargo_bin("trace-diff")
            .unwrap()
            .args(["list", "--baselines-only", "--db"])
            .arg(&db)
            .assert()
            .success()
            .stdout(predicate::str::contains("ci-baseline"));

        Command::cargo_bin("trace-diff")
            .unwrap()
            .args([
                "diff",
                "ci-baseline",
                "--skip-trace",
                "--headless",
                "--db",
            ])
            .arg(&db)
            .assert()
            .success()
            .stdout(predicate::str::contains("baseline"));
    });
}
