# check=skip=SecretsUsedInArgOrEnv
# (the ENV values below are file paths and ports, not secrets; the linter
# matches on the word TOKEN in GOOGLE_ADS_TOKEN_PATH)

# Build a release binary, then ship it on a slim runtime image.
# The image serves MCP over Streamable HTTP on port 8080 at /mcp, the same
# layout as the official googleads/google-ads-mcp image.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /var/lib/mcp-google-ads --create-home mcp
COPY --from=build /src/target/release/mcp-google-ads /usr/local/bin/mcp-google-ads

# Inside a container the server must listen on all interfaces, and the
# container's loopback is not the host's, so the transport is fixed here.
# Credentials, developer token and the bearer token are NOT baked in: pass
# them at run time (`--env-file`, Cloud Run env vars, a Quadlet
# EnvironmentFile=).
ENV GOOGLE_ADS_MCP_TRANSPORT=streamable-http \
    GOOGLE_ADS_MCP_HOST=0.0.0.0 \
    PORT=3000 \
    GOOGLE_ADS_CREDENTIALS_PATH=/var/lib/mcp-google-ads/credentials.json \
    GOOGLE_ADS_TOKEN_PATH=/var/lib/mcp-google-ads/token.json \
    GOOGLE_ADS_AUDIT_LOG=/var/lib/mcp-google-ads/audit.log

USER mcp
WORKDIR /var/lib/mcp-google-ads
VOLUME ["/var/lib/mcp-google-ads"]
EXPOSE 3000
# No curl in the runtime image; bash's /dev/tcp is enough for a GET /health.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
    CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/${PORT} && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q ' 200 ' <&3"]

CMD ["mcp-google-ads"]
