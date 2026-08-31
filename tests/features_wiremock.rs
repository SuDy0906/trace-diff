//! `trace-diff features -y` against wiremock OpenAPI.

use assert_cmd::Command;
use predicates::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OPENAPI: &str = r#"{
  "openapi": "3.0.0",
  "info": { "title": "CI Test API", "version": "1.0.0" },
  "paths": {
    "/health": {
      "get": {
        "tags": ["health"],
        "responses": { "200": { "description": "ok" } }
      }
    },
    "/api/items": {
      "get": {
        "tags": ["items"],
        "responses": { "200": { "description": "ok" } }
      }
    }
  }
}"#;

#[test]
fn features_headless_discovers_wiremock_openapi() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openapi.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OPENAPI))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/items"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&server)
            .await;

        let base = format!("{}/", server.uri());
        Command::cargo_bin("trace-diff")
            .unwrap()
            .args([
                "features",
                &base,
                "-y",
                "--no-llm",
                "--no-tls-canary",
                "--json",
            ])
            .assert()
            .success()
            .stdout(predicates::str::contains("results"));
    });
}

#[test]
fn check_llm_json_output() {
    Command::cargo_bin("trace-diff")
        .unwrap()
        .args(["features", "--check-llm", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"ready\""));
}

#[test]
fn features_ci_gate_fail_on_reachable() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/openapi.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OPENAPI))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(401).set_body_string("auth required"))
            .mount(&server)
            .await;

        let base = format!("{}/", server.uri());
        Command::cargo_bin("trace-diff")
            .unwrap()
            .args([
                "features",
                &base,
                "-y",
                "--no-llm",
                "--no-tls-canary",
                "--fail-on-reachable",
                "--max-features",
                "1",
            ])
            .assert()
            .failure()
            .stderr(predicates::str::contains("reachable").or(predicates::str::contains("failed")));
    });
}
