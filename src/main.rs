use mcp_google_ads::config::{Config, Transport};
use mcp_google_ads::error::{McpGoogleAdsError, Result};

use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = Config::load()?;
    tracing::info!(
        "Starting MCP Google Ads server ({} transport)",
        config.transport.as_str()
    );

    let transport = config.transport;
    let http = config.http.clone();
    let server = mcp_google_ads::GoogleAdsMcp::new(config)?;

    match transport {
        Transport::Stdio => serve_stdio(server).await,
        Transport::StreamableHttp => mcp_google_ads::http::serve(server, &http).await,
    }
}

async fn serve_stdio(server: mcp_google_ads::GoogleAdsMcp) -> Result<()> {
    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| McpGoogleAdsError::Config(format!("Failed to start MCP server: {}", e)))?;

    service
        .waiting()
        .await
        .map_err(|e| McpGoogleAdsError::Config(format!("Server error: {}", e)))?;

    Ok(())
}
