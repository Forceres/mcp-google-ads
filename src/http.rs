//! MCP Streamable HTTP transport.
//!
//! Under stdio the MCP client launches this binary and owns its lifetime. Under
//! Streamable HTTP the server runs on its own, and clients connect to
//! `http://<host>:<port>/mcp` — the same endpoint layout as the official
//! `googleads/google-ads-mcp` server, so a client configured for one can point
//! at the other by changing only the URL.
//!
//! What this adds on top of rmcp's `StreamableHttpService`:
//!
//! - an optional static bearer token in front of the MCP endpoint. Every tool
//!   on this server can spend money, so a network-reachable instance should
//!   never be anonymous. The token is checked in constant time.
//! - rmcp's `Host` validation (DNS-rebinding protection) wired to
//!   configuration. The default accepts loopback hosts only; a public
//!   deployment lists its own hostname.
//! - a `GET /health` endpoint, unauthenticated, for container and load-balancer
//!   probes.
//! - graceful shutdown on SIGINT/SIGTERM that also closes open MCP sessions.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use crate::config::HttpConfig;
use crate::error::{McpGoogleAdsError, Result};
use crate::GoogleAdsMcp;

/// Path of the MCP endpoint. Fixed, like the official server's.
pub const MCP_PATH: &str = "/mcp";
/// Path of the liveness probe.
pub const HEALTH_PATH: &str = "/health";

/// Build the HTTP application: `/mcp` (optionally behind a bearer token) and
/// `/health`.
///
/// `cancel` is handed to rmcp: cancelling it terminates every open session and
/// makes the MCP endpoint stop accepting requests, which is what graceful
/// shutdown needs. [`serve`] creates one; tests can pass their own.
pub fn router(server: GoogleAdsMcp, http: &HttpConfig, cancel: CancellationToken) -> Router {
    let mut rmcp_config = StreamableHttpServerConfig::default().with_cancellation_token(cancel);
    if http.accepts_any_host() {
        rmcp_config = rmcp_config.disable_allowed_hosts();
    } else if !http.allowed_hosts.is_empty() {
        rmcp_config = rmcp_config.with_allowed_hosts(http.allowed_hosts.iter().cloned());
    }
    if !http.allowed_origins.is_empty() {
        rmcp_config = rmcp_config.with_allowed_origins(http.allowed_origins.iter().cloned());
    }

    // Every session gets its own handler clone. The handler holds only the
    // immutable configuration and the tool router, so a clone is cheap and
    // sessions cannot observe each other.
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        rmcp_config,
    );

    let mut mcp = Router::new().route_service(MCP_PATH, service);
    if let Some(token) = &http.bearer_token {
        mcp = mcp.layer(middleware::from_fn_with_state(
            Arc::new(token.clone()),
            require_bearer,
        ));
    }

    Router::new()
        .route(HEALTH_PATH, get(|| async { "ok" }))
        .merge(mcp)
}

/// Bind `host:port` and serve until SIGINT or SIGTERM.
pub async fn serve(server: GoogleAdsMcp, http: &HttpConfig) -> Result<()> {
    warn_about_exposure(http);

    let cancel = CancellationToken::new();
    let app = router(server, http, cancel.clone());

    let listener = tokio::net::TcpListener::bind((http.host.as_str(), http.port))
        .await
        .map_err(|e| {
            McpGoogleAdsError::Config(format!("Cannot listen on {}:{}: {e}", http.host, http.port))
        })?;
    let local = listener.local_addr().map_err(McpGoogleAdsError::Io)?;
    tracing::info!(
        "MCP Streamable HTTP endpoint at http://{local}{MCP_PATH} (health at {HEALTH_PATH})"
    );

    let shutdown = async move {
        shutdown_signal().await;
        tracing::info!("Shutdown requested, closing MCP sessions");
        cancel.cancel();
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| McpGoogleAdsError::Config(format!("HTTP server error: {e}")))
}

/// Say loudly, once at startup, when the configuration leaves a money-spending
/// endpoint reachable without credentials.
fn warn_about_exposure(http: &HttpConfig) {
    if http.bearer_token.is_none() && !http.binds_loopback() {
        tracing::warn!(
            "Listening on {} with no GOOGLE_ADS_MCP_BEARER_TOKEN: anyone who can reach this \
             port can drive the Google Ads account",
            http.host
        );
    }
    if http.accepts_any_host() {
        tracing::warn!(
            "GOOGLE_ADS_MCP_ALLOWED_HOSTS=* disables Host validation; set it to the server's \
             real hostname for deployments reachable from a browser"
        );
    }
}

async fn require_bearer(
    State(expected): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, rest) = value.trim().split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
        });

    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
            next.run(request).await
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, "Bearer realm=\"mcp-google-ads\"")],
            "Unauthorized: a valid `Authorization: Bearer <token>` header is required",
        )
            .into_response(),
    }
}

/// Compare without short-circuiting on the first differing byte, so response
/// time does not leak how much of the token was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("Cannot listen for Ctrl-C: {e}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                tracing::error!("Cannot listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"tokem"));
        assert!(!constant_time_eq(b"token", b"toke"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }
}
