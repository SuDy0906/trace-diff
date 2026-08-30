//! L7 delayed-phase and connection-fault integration tests (wiremock).

use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::sleep;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn timed_get(url: &str) -> Duration {
    let start = std::time::Instant::now();
    let _ = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .get(url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    start.elapsed()
}

#[tokio::test]
async fn wiremock_delayed_ttfb_is_observable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(120)))
        .mount(&server)
        .await;

    let url = format!("{}/slow", server.uri());
    let elapsed = timed_get(&url).await;
    assert!(
        elapsed >= Duration::from_millis(100),
        "expected delayed response, got {elapsed:?}"
    );
}

#[tokio::test]
async fn wiremock_fast_path_under_budget() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/fast"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let url = format!("{}/fast", server.uri());
    let elapsed = timed_get(&url).await;
    assert!(
        elapsed < Duration::from_millis(500),
        "fast path too slow: {elapsed:?}"
    );
}

#[tokio::test]
async fn local_connection_refused_surfaces_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    drop(listener);
    sleep(Duration::from_millis(20)).await;

    let url = format!("http://{addr}/");
    let res = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(&url)
        .send()
        .await;
    assert!(res.is_err(), "expected connection error to closed port");
}

#[tokio::test]
async fn l7_probe_against_wiremock_http() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(50))
                .set_body_string("hello"),
        )
        .mount(&server)
        .await;

    let metrics = trace_diff::l7::probe(&server.uri(), trace_diff::l7::L7Config::default())
        .await
        .expect("l7 probe");
    assert_eq!(metrics.status, Some(200));
    assert!(metrics.ttfb_ms.unwrap_or(0.0) >= 40.0);
    assert!(metrics.total_ms >= metrics.ttfb_ms.unwrap_or(0.0));
}
