use std::path::PathBuf;

use crate::error::{McpGoogleAdsError, Result};

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_path_or(key: &str, default: &str) -> PathBuf {
    expand_tilde(&env_or(key, default))
}

fn env_f64_or(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32_or(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool_or(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(default)
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct GoogleConfig {
    pub credentials_path: PathBuf,
    pub token_path: PathBuf,
}

impl Default for GoogleConfig {
    fn default() -> Self {
        Self {
            credentials_path: expand_tilde("~/.mcp-google-ads/credentials.json"),
            token_path: expand_tilde("~/.mcp-google-ads/token.json"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdsConfig {
    pub developer_token: String,
    pub customer_id: String,
    pub login_customer_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SafetyConfig {
    pub max_daily_budget: f64,
    pub max_bid_increase_pct: u32,
    pub require_dry_run: bool,
    pub log_file: PathBuf,
    pub blocked_operations: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            max_daily_budget: 50.0,
            max_bid_increase_pct: 100,
            require_dry_run: true,
            log_file: expand_tilde("~/.mcp-google-ads/audit.log"),
            blocked_operations: Vec::new(),
        }
    }
}

/// Which MCP transport the binary serves on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// JSON-RPC over the process's stdin/stdout. The client launches the binary.
    #[default]
    Stdio,
    /// MCP Streamable HTTP. The server runs on its own and clients connect to
    /// `http://<host>:<port>/mcp`.
    StreamableHttp,
}

impl Transport {
    /// Parse the `GOOGLE_ADS_MCP_TRANSPORT` value.
    ///
    /// `streamable-http` is the name the MCP specification and the official
    /// `googleads/google-ads-mcp` server use; `http` is accepted as a
    /// shorthand. An unknown value is an error rather than a silent fallback
    /// to stdio, because a server that was meant to listen on a port and
    /// instead blocks on stdin looks hung.
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "stdio" => Ok(Self::Stdio),
            "streamable-http" | "streamable_http" | "http" => Ok(Self::StreamableHttp),
            other => Err(McpGoogleAdsError::Config(format!(
                "GOOGLE_ADS_MCP_TRANSPORT must be `stdio` or `streamable-http`, got `{other}`"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::StreamableHttp => "streamable-http",
        }
    }
}

/// Settings for the Streamable HTTP transport. Ignored under stdio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// Interface to bind. `127.0.0.1` by default so a local server is not
    /// reachable from the network by accident; containers set `0.0.0.0`.
    pub host: String,
    /// TCP port. Read from `PORT`, the variable Cloud Run and most PaaS
    /// platforms inject, defaulting to 8080 like the official server.
    pub port: u16,
    /// When set, every request to the MCP endpoint must carry
    /// `Authorization: Bearer <token>`. The health endpoint stays open.
    pub bearer_token: Option<String>,
    /// `Host` header values the server accepts, as DNS-rebinding protection.
    /// Empty means loopback only (`localhost`, `127.0.0.1`, `::1`); a single
    /// `*` disables the check.
    pub allowed_hosts: Vec<String>,
    /// Browser `Origin` values the server accepts. Empty disables the check.
    pub allowed_origins: Vec<String>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            bearer_token: None,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
        }
    }
}

impl HttpConfig {
    /// `true` when `allowed_hosts` is the `*` wildcard, i.e. any `Host` passes.
    pub fn accepts_any_host(&self) -> bool {
        self.allowed_hosts.len() == 1 && self.allowed_hosts[0] == "*"
    }

    /// `true` when the bind address only answers on the local machine.
    pub fn binds_loopback(&self) -> bool {
        matches!(self.host.as_str(), "127.0.0.1" | "::1" | "localhost")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub google: GoogleConfig,
    pub ads: AdsConfig,
    pub safety: SafetyConfig,
    /// When true, only READ tools are exposed. All write/mutate tools are hidden.
    pub read_only: bool,
    /// Transport the binary serves on. Defaults to stdio.
    pub transport: Transport,
    /// Streamable HTTP settings, used when `transport` is [`Transport::StreamableHttp`].
    pub http: HttpConfig,
}

impl Config {
    /// Load configuration entirely from environment variables.
    ///
    /// | Env var | Default |
    /// |---|---|
    /// | `GOOGLE_ADS_CREDENTIALS_PATH` | `~/.mcp-google-ads/credentials.json` |
    /// | `GOOGLE_ADS_TOKEN_PATH` | `~/.mcp-google-ads/token.json` |
    /// | `GOOGLE_ADS_DEVELOPER_TOKEN` | (empty) |
    /// | `GOOGLE_ADS_CUSTOMER_ID` | (empty) |
    /// | `GOOGLE_ADS_LOGIN_CUSTOMER_ID` | (none) |
    /// | `GOOGLE_ADS_MAX_DAILY_BUDGET` | `50.0` |
    /// | `GOOGLE_ADS_MAX_BID_INCREASE_PCT` | `100` |
    /// | `GOOGLE_ADS_REQUIRE_DRY_RUN` | `true` |
    /// | `GOOGLE_ADS_AUDIT_LOG` | `~/.mcp-google-ads/audit.log` |
    /// | `GOOGLE_ADS_BLOCKED_OPS` | (empty, comma-separated) |
    /// | `GOOGLE_ADS_READ_ONLY` | `false` |
    /// | `GOOGLE_ADS_MCP_TRANSPORT` | `stdio` (or `streamable-http`) |
    /// | `GOOGLE_ADS_MCP_HOST` | `127.0.0.1` |
    /// | `PORT` | `8080` |
    /// | `GOOGLE_ADS_MCP_BEARER_TOKEN` | (none) |
    /// | `GOOGLE_ADS_MCP_ALLOWED_HOSTS` | (loopback only, comma-separated, `*` for any) |
    /// | `GOOGLE_ADS_MCP_ALLOWED_ORIGINS` | (empty, comma-separated) |
    pub fn load() -> Result<Self> {
        let login_customer_id = std::env::var("GOOGLE_ADS_LOGIN_CUSTOMER_ID")
            .ok()
            .filter(|v| !v.is_empty());

        let transport = Transport::parse(&env_or("GOOGLE_ADS_MCP_TRANSPORT", "stdio"))?;
        let port = match std::env::var("PORT") {
            Ok(raw) if !raw.trim().is_empty() => raw.trim().parse::<u16>().map_err(|_| {
                McpGoogleAdsError::Config(format!(
                    "PORT must be a number from 1 to 65535, got `{raw}`"
                ))
            })?,
            _ => 8080,
        };
        let http = HttpConfig {
            host: env_or("GOOGLE_ADS_MCP_HOST", "127.0.0.1"),
            port,
            bearer_token: std::env::var("GOOGLE_ADS_MCP_BEARER_TOKEN")
                .ok()
                .filter(|v| !v.is_empty()),
            allowed_hosts: env_list("GOOGLE_ADS_MCP_ALLOWED_HOSTS"),
            allowed_origins: env_list("GOOGLE_ADS_MCP_ALLOWED_ORIGINS"),
        };

        Ok(Config {
            google: GoogleConfig {
                credentials_path: env_path_or(
                    "GOOGLE_ADS_CREDENTIALS_PATH",
                    "~/.mcp-google-ads/credentials.json",
                ),
                token_path: env_path_or("GOOGLE_ADS_TOKEN_PATH", "~/.mcp-google-ads/token.json"),
            },
            ads: AdsConfig {
                developer_token: env_or("GOOGLE_ADS_DEVELOPER_TOKEN", ""),
                customer_id: env_or("GOOGLE_ADS_CUSTOMER_ID", ""),
                login_customer_id,
            },
            safety: SafetyConfig {
                max_daily_budget: env_f64_or("GOOGLE_ADS_MAX_DAILY_BUDGET", 50.0),
                max_bid_increase_pct: env_u32_or("GOOGLE_ADS_MAX_BID_INCREASE_PCT", 100),
                require_dry_run: env_bool_or("GOOGLE_ADS_REQUIRE_DRY_RUN", true),
                log_file: env_path_or("GOOGLE_ADS_AUDIT_LOG", "~/.mcp-google-ads/audit.log"),
                blocked_operations: env_list("GOOGLE_ADS_BLOCKED_OPS"),
            },
            read_only: env_bool_or("GOOGLE_ADS_READ_ONLY", false),
            transport,
            http,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// `Config::load` reads process-wide environment variables, and the harness
    /// runs tests as concurrent threads inside one process. Without
    /// serialisation, one test's `remove_var` lands between another's
    /// `set_var` and its `Config::load`, so the second reads a value it never
    /// set. Every test that drives `Config::load` through the environment takes
    /// this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// A panicking test must not wedge the others, so a poisoned lock is
    /// recovered instead of propagated.
    fn env_guard() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.safety.max_daily_budget, 50.0);
        assert_eq!(config.safety.max_bid_increase_pct, 100);
        assert!(config.safety.require_dry_run);
        assert!(config.safety.blocked_operations.is_empty());
    }

    #[test]
    fn test_expand_tilde() {
        let path = expand_tilde("~/.mcp-google-ads/config.yaml");
        assert!(!path.to_str().unwrap_or_default().starts_with('~'));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        let path = expand_tilde("/absolute/path");
        assert_eq!(path, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_env_or_default() {
        assert_eq!(
            env_or("__MCP_TEST_NONEXISTENT_VAR__", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn test_env_f64_or_default() {
        assert_eq!(env_f64_or("__MCP_TEST_NONEXISTENT_VAR__", 42.5), 42.5);
    }

    #[test]
    fn test_env_u32_or_default() {
        assert_eq!(env_u32_or("__MCP_TEST_NONEXISTENT_VAR__", 200), 200);
    }

    #[test]
    fn test_env_bool_or_default() {
        assert!(env_bool_or("__MCP_TEST_NONEXISTENT_VAR__", true));
        assert!(!env_bool_or("__MCP_TEST_NONEXISTENT_VAR__", false));
    }

    #[test]
    fn test_env_list_empty() {
        let list = env_list("__MCP_TEST_NONEXISTENT_VAR__");
        assert!(list.is_empty());
    }

    #[test]
    fn test_load_defaults() {
        let _env = env_guard();

        // Clear any env vars that could interfere
        std::env::remove_var("GOOGLE_ADS_DEVELOPER_TOKEN");
        std::env::remove_var("GOOGLE_ADS_CUSTOMER_ID");
        std::env::remove_var("GOOGLE_ADS_LOGIN_CUSTOMER_ID");
        std::env::remove_var("GOOGLE_ADS_MAX_DAILY_BUDGET");
        std::env::remove_var("GOOGLE_ADS_MAX_BID_INCREASE_PCT");
        std::env::remove_var("GOOGLE_ADS_REQUIRE_DRY_RUN");
        std::env::remove_var("GOOGLE_ADS_BLOCKED_OPS");

        let config = Config::load().unwrap();
        assert_eq!(config.ads.developer_token, "");
        assert_eq!(config.ads.customer_id, "");
        assert!(config.ads.login_customer_id.is_none());
        assert_eq!(config.safety.max_daily_budget, 50.0);
        assert_eq!(config.safety.max_bid_increase_pct, 100);
        assert!(config.safety.require_dry_run);
        assert!(config.safety.blocked_operations.is_empty());
    }

    #[test]
    fn test_load_from_env() {
        let _env = env_guard();

        std::env::set_var("GOOGLE_ADS_DEVELOPER_TOKEN", "test-dev-token");
        std::env::set_var("GOOGLE_ADS_CUSTOMER_ID", "123-456-7890");
        std::env::set_var("GOOGLE_ADS_LOGIN_CUSTOMER_ID", "999-888-7777");
        std::env::set_var("GOOGLE_ADS_MAX_DAILY_BUDGET", "200.0");
        std::env::set_var("GOOGLE_ADS_MAX_BID_INCREASE_PCT", "50");
        std::env::set_var("GOOGLE_ADS_REQUIRE_DRY_RUN", "false");
        std::env::set_var("GOOGLE_ADS_BLOCKED_OPS", "delete_campaign,remove_entity");
        std::env::set_var("GOOGLE_ADS_CREDENTIALS_PATH", "/tmp/creds.json");
        std::env::set_var("GOOGLE_ADS_TOKEN_PATH", "/tmp/token.json");
        std::env::set_var("GOOGLE_ADS_AUDIT_LOG", "/tmp/audit.log");

        let config = Config::load().unwrap();
        assert_eq!(config.ads.developer_token, "test-dev-token");
        assert_eq!(config.ads.customer_id, "123-456-7890");
        assert_eq!(
            config.ads.login_customer_id,
            Some("999-888-7777".to_string())
        );
        assert_eq!(config.safety.max_daily_budget, 200.0);
        assert_eq!(config.safety.max_bid_increase_pct, 50);
        assert!(!config.safety.require_dry_run);
        assert_eq!(
            config.safety.blocked_operations,
            vec!["delete_campaign", "remove_entity"]
        );
        assert_eq!(
            config.google.credentials_path,
            PathBuf::from("/tmp/creds.json")
        );
        assert_eq!(config.google.token_path, PathBuf::from("/tmp/token.json"));
        assert_eq!(config.safety.log_file, PathBuf::from("/tmp/audit.log"));

        // Clean up
        std::env::remove_var("GOOGLE_ADS_DEVELOPER_TOKEN");
        std::env::remove_var("GOOGLE_ADS_CUSTOMER_ID");
        std::env::remove_var("GOOGLE_ADS_LOGIN_CUSTOMER_ID");
        std::env::remove_var("GOOGLE_ADS_MAX_DAILY_BUDGET");
        std::env::remove_var("GOOGLE_ADS_MAX_BID_INCREASE_PCT");
        std::env::remove_var("GOOGLE_ADS_REQUIRE_DRY_RUN");
        std::env::remove_var("GOOGLE_ADS_BLOCKED_OPS");
        std::env::remove_var("GOOGLE_ADS_CREDENTIALS_PATH");
        std::env::remove_var("GOOGLE_ADS_TOKEN_PATH");
        std::env::remove_var("GOOGLE_ADS_AUDIT_LOG");
    }

    #[test]
    fn test_bool_parsing_variants() {
        std::env::set_var("__MCP_TEST_BOOL__", "true");
        assert!(env_bool_or("__MCP_TEST_BOOL__", false));
        std::env::set_var("__MCP_TEST_BOOL__", "1");
        assert!(env_bool_or("__MCP_TEST_BOOL__", false));
        std::env::set_var("__MCP_TEST_BOOL__", "yes");
        assert!(env_bool_or("__MCP_TEST_BOOL__", false));
        std::env::set_var("__MCP_TEST_BOOL__", "false");
        assert!(!env_bool_or("__MCP_TEST_BOOL__", true));
        std::env::set_var("__MCP_TEST_BOOL__", "0");
        assert!(!env_bool_or("__MCP_TEST_BOOL__", true));
        std::env::set_var("__MCP_TEST_BOOL__", "no");
        assert!(!env_bool_or("__MCP_TEST_BOOL__", true));
        std::env::remove_var("__MCP_TEST_BOOL__");
    }

    #[test]
    fn test_env_list_parsing() {
        std::env::set_var("__MCP_TEST_LIST__", "a, b , c");
        let list = env_list("__MCP_TEST_LIST__");
        assert_eq!(list, vec!["a", "b", "c"]);
        std::env::remove_var("__MCP_TEST_LIST__");
    }

    #[test]
    fn test_login_customer_id_empty_string() {
        let _env = env_guard();

        std::env::set_var("GOOGLE_ADS_LOGIN_CUSTOMER_ID", "");
        let config = Config::load().unwrap();
        assert!(config.ads.login_customer_id.is_none());
        std::env::remove_var("GOOGLE_ADS_LOGIN_CUSTOMER_ID");
    }

    #[test]
    fn test_transport_parse() {
        assert_eq!(Transport::parse("").unwrap(), Transport::Stdio);
        assert_eq!(Transport::parse("stdio").unwrap(), Transport::Stdio);
        assert_eq!(
            Transport::parse("streamable-http").unwrap(),
            Transport::StreamableHttp
        );
        assert_eq!(
            Transport::parse(" HTTP ").unwrap(),
            Transport::StreamableHttp
        );
        let err = Transport::parse("sse").unwrap_err().to_string();
        assert!(err.contains("GOOGLE_ADS_MCP_TRANSPORT"), "got: {err}");
    }

    #[test]
    fn test_load_transport_defaults_to_stdio_on_loopback_8080() {
        let _env = env_guard();
        for key in [
            "GOOGLE_ADS_MCP_TRANSPORT",
            "GOOGLE_ADS_MCP_HOST",
            "PORT",
            "GOOGLE_ADS_MCP_BEARER_TOKEN",
            "GOOGLE_ADS_MCP_ALLOWED_HOSTS",
            "GOOGLE_ADS_MCP_ALLOWED_ORIGINS",
        ] {
            std::env::remove_var(key);
        }

        let config = Config::load().unwrap();
        assert_eq!(config.transport, Transport::Stdio);
        assert_eq!(config.http, HttpConfig::default());
        assert!(config.http.binds_loopback());
        assert!(!config.http.accepts_any_host());
    }

    #[test]
    fn test_load_streamable_http_from_env() {
        let _env = env_guard();
        std::env::set_var("GOOGLE_ADS_MCP_TRANSPORT", "streamable-http");
        std::env::set_var("GOOGLE_ADS_MCP_HOST", "0.0.0.0");
        std::env::set_var("PORT", "9090");
        std::env::set_var("GOOGLE_ADS_MCP_BEARER_TOKEN", "s3cret");
        std::env::set_var("GOOGLE_ADS_MCP_ALLOWED_HOSTS", "ads.example.com, localhost");
        std::env::set_var("GOOGLE_ADS_MCP_ALLOWED_ORIGINS", "https://app.example.com");

        let config = Config::load().unwrap();
        assert_eq!(config.transport, Transport::StreamableHttp);
        assert_eq!(config.http.host, "0.0.0.0");
        assert_eq!(config.http.port, 9090);
        assert_eq!(config.http.bearer_token.as_deref(), Some("s3cret"));
        assert_eq!(
            config.http.allowed_hosts,
            vec!["ads.example.com", "localhost"]
        );
        assert_eq!(config.http.allowed_origins, vec!["https://app.example.com"]);
        assert!(!config.http.binds_loopback());

        std::env::set_var("GOOGLE_ADS_MCP_ALLOWED_HOSTS", "*");
        assert!(Config::load().unwrap().http.accepts_any_host());

        for key in [
            "GOOGLE_ADS_MCP_TRANSPORT",
            "GOOGLE_ADS_MCP_HOST",
            "PORT",
            "GOOGLE_ADS_MCP_BEARER_TOKEN",
            "GOOGLE_ADS_MCP_ALLOWED_HOSTS",
            "GOOGLE_ADS_MCP_ALLOWED_ORIGINS",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn test_load_rejects_bad_transport_and_port() {
        let _env = env_guard();
        std::env::set_var("GOOGLE_ADS_MCP_TRANSPORT", "sse");
        assert!(Config::load().is_err());
        std::env::remove_var("GOOGLE_ADS_MCP_TRANSPORT");

        std::env::set_var("PORT", "eighty");
        let err = Config::load().unwrap_err().to_string();
        assert!(err.contains("PORT"), "got: {err}");
        std::env::remove_var("PORT");
    }
}
