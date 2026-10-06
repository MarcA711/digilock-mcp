use std::{future::IntoFuture, time::Duration};

use anyhow::{Context, Result};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::net::TcpListener;

use crate::server::DigiLockServer;

pub async fn serve(
    listener: TcpListener,
    server: DigiLockServer,
    additional_hosts: Vec<String>,
) -> Result<()> {
    let address = listener
        .local_addr()
        .context("reading MCP listen address")?;
    let mut hosts = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    if !address.ip().is_unspecified() {
        hosts.push(address.ip().to_string());
    }
    hosts.extend(additional_hosts);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(hosts)
        // Native MCP clients do not send Origin. Reject browser-origin requests
        // rather than letting a web page access unauthenticated hardware tools.
        .enforce_origin_validation();
    let cancellation = config.cancellation_token.clone();
    // Clones share the RCI client and its per-port transaction locks. Creating
    // an independent client for each MCP session would violate RCI exclusivity.
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        config,
    );
    let router = axum::Router::new().route_service("/mcp", service);
    let shutdown = cancellation.clone();
    let running = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .into_future();
    tokio::pin!(running);
    eprintln!("MCP network service listening on http://{address}/mcp");
    eprintln!("Network MCP has no authentication or TLS; restrict access to trusted clients.");
    tokio::select! {
        result = &mut running => result.context("running MCP network service")?,
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for shutdown signal")?;
            cancellation.cancel();
            tokio::time::timeout(Duration::from_secs(2), &mut running)
                .await.context("timed out shutting down MCP network service")?
                .context("shutting down MCP network service")?;
        }
    }
    Ok(())
}
