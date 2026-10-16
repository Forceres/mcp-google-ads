//! The Streamable HTTP transport must speak MCP end to end over a real socket.
//!
//! These tests bind an ephemeral port, run the same router the binary serves,
//! and talk to it with a plain HTTP client the way any MCP client would:
//! `initialize`, `notifications/initialized`, `tools/list`. No Google Ads
//! credentials are involved — nothing past the tool list is exercised.
//!
//! They also pin the two things that make an HTTP deployment safe to expose:
//! the bearer token in front of `/mcp` (while `/health` stays open for probes)
//! and the `Host` check that stops DNS-rebinding attacks on a local server.

mod common;

use mcp_google_ads::config::HttpConfig;
use mcp_google_ads::http::{router, HEALTH_PATH, MCP_PATH};
use mcp_google_ads::GoogleAdsMcp;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// A running server. Dropping it shuts the server down.
struct Running {
    base: String,
    cancel: CancellationToken,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn spawn(http: HttpConfig) -> Running {
    let server = GoogleAdsMcp::new(common::test_config()).expect("server builds");
    let cancel = CancellationToken::new();
    let app = router(server, &http, cancel.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port is free");
    let addr = listener.local_addr().unwrap();

    let shutdown = cancel.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .expect("server runs");
    });

    Running {
        base: format!("http://{addr}"),
        cancel,
    }
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "transport-test", "version": "0"}
        }
    })
}

/// Pull the JSON-RPC message out of a Streamable HTTP response body, whether
/// the server chose `application/json` or an SSE stream.
fn first_json_message(content_type: &str, body: &str) -> Value {
    if content_type.starts_with("text/event-stream") {
        // The stream may open with a priming event (empty data, a retry
        // hint) before the first JSON-RPC message.
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .find(|value| value.get("jsonrpc").is_some())
            .unwrap_or_else(|| panic!("no JSON-RPC message in SSE body: {body}"))
    } else {
        serde_json::from_str(body).expect("body is JSON")
    }
}

fn post(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
}

#[tokio::test]
async fn initialize_then_list_tools_over_http() {
    let running = spawn(HttpConfig::default()).await;
    let client = reqwest::Client::new();
    let url = format!("{}{MCP_PATH}", running.base);

    let response = post(&client, &url)
        .json(&initialize_request())
        .send()
        .await
        .expect("initialize reaches the server");
    assert_eq!(response.status(), StatusCode::OK);
    let session = response
        .headers()
        .get("mcp-session-id")
        .expect("the server assigns a session")
        .to_str()
        .unwrap()
        .to_string();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().await.unwrap();
    let message = first_json_message(&content_type, &body);
    assert_eq!(
        message["result"]["serverInfo"]["name"], "mcp-google-ads",
        "clients match configuration against the published name, got: {message}"
    );
    assert!(
        message["result"]["capabilities"]["tools"].is_object(),
        "tools capability advertised, got: {message}"
    );

    let response = post(&client, &url)
        .header("mcp-session-id", &session)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "a notification is acknowledged with 202"
    );

    let response = post(&client, &url)
        .header("mcp-session-id", &session)
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().await.unwrap();
    let message = first_json_message(&content_type, &body);
    let names: Vec<&str> = message["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list returns a list, got: {message}"))
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(
        names.contains(&"list_accounts") && names.contains(&"health_check"),
        "the same tools as stdio are served over HTTP, got: {names:?}"
    );

    // Closing the session is part of the protocol; a client that cannot is
    // leaking server memory.
    let response = client
        .delete(&url)
        .header("mcp-session-id", &session)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "DELETE closes the session, got {}",
        response.status()
    );
}

#[tokio::test]
async fn bearer_token_guards_mcp_but_not_health() {
    let running = spawn(HttpConfig {
        bearer_token: Some("correct-horse".to_string()),
        ..HttpConfig::default()
    })
    .await;
    let client = reqwest::Client::new();
    let url = format!("{}{MCP_PATH}", running.base);

    let anonymous = post(&client, &url)
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert!(
        anonymous.headers().contains_key("www-authenticate"),
        "a 401 tells the client which scheme to use"
    );

    let wrong = post(&client, &url)
        .header(AUTHORIZATION, "Bearer correct-horsf")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let basic = post(&client, &url)
        .header(AUTHORIZATION, "Basic Y29ycmVjdC1ob3JzZQ==")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(
        basic.status(),
        StatusCode::UNAUTHORIZED,
        "only Bearer counts"
    );

    let right = post(&client, &url)
        .header(AUTHORIZATION, "Bearer correct-horse")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), StatusCode::OK);

    let health = client
        .get(format!("{}{HEALTH_PATH}", running.base))
        .send()
        .await
        .unwrap();
    assert_eq!(
        health.status(),
        StatusCode::OK,
        "probes carry no credentials"
    );
    assert_eq!(health.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn foreign_host_header_is_rejected_by_default() {
    let running = spawn(HttpConfig::default()).await;
    let client = reqwest::Client::new();
    let url = format!("{}{MCP_PATH}", running.base);

    let rebinding = post(&client, &url)
        .header("host", "ads.attacker.example")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(
        rebinding.status(),
        StatusCode::FORBIDDEN,
        "a browser on a rebound DNS name must not reach a loopback server"
    );
}

#[tokio::test]
async fn allowed_hosts_opens_a_named_host_and_star_opens_all() {
    let named = spawn(HttpConfig {
        allowed_hosts: vec!["ads.example.com".to_string()],
        ..HttpConfig::default()
    })
    .await;
    let client = reqwest::Client::new();

    let ok = post(&client, &format!("{}{MCP_PATH}", named.base))
        .header("host", "ads.example.com")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    let other = post(&client, &format!("{}{MCP_PATH}", named.base))
        .header("host", "other.example.com")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::FORBIDDEN);

    let open = spawn(HttpConfig {
        allowed_hosts: vec!["*".to_string()],
        ..HttpConfig::default()
    })
    .await;
    let any = post(&client, &format!("{}{MCP_PATH}", open.base))
        .header("host", "whatever.example")
        .json(&initialize_request())
        .send()
        .await
        .unwrap();
    assert_eq!(any.status(), StatusCode::OK);
}
