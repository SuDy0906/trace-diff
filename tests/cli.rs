use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

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
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");

    Command::cargo_bin("trace-diff")
        .unwrap()
        .args([
            "run",
            "https://example.com",
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
}
