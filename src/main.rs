mod catalog;
mod config;
mod network;
mod policy;
mod rci;
mod server;

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use rmcp::{ServiceExt, transport::stdio};
use tokio::net::TcpListener;

use server::{Config, DigiLockServer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Transport {
    Stdio,
    Network,
}

/// MCP server for the TOPTICA DigiLock 110 TCP Remote Control Interface.
/// The Windows DigiLock Module Server and the desired DUI must already be running.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// MCP transport: stdio or Streamable HTTP at /mcp.
    #[arg(long, env = "DIGILOCK_TRANSPORT", value_enum, default_value = "stdio")]
    transport: Transport,
    /// Network MCP listen address (separate from the Windows RCI host).
    #[arg(long, env = "DIGILOCK_LISTEN", default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
    /// Additional accepted HTTP Host (repeat for DNS names or wildcard-bind access).
    #[arg(long)]
    allowed_host: Vec<String>,
    /// Host running the Windows DigiLock software (not the USB module itself).
    #[arg(long, env = "DIGILOCK_HOST", default_value = "127.0.0.1")]
    host: String,
    /// Module Server RCI port.
    #[arg(long, env = "DIGILOCK_DMS_PORT", default_value_t = 60000)]
    dms_port: u16,
    /// Default User Interface RCI port; MCP requests can select another DUI port.
    #[arg(long, env = "DIGILOCK_DUI_PORT", default_value_t = 60001)]
    dui_port: u16,
    /// Whole-transaction timeout in milliseconds, including queue wait and welcome.
    #[arg(long, env = "DIGILOCK_TIMEOUT_MS", default_value_t = 5000)]
    timeout_ms: u64,
    /// Maximum bytes in each welcome or response frame (increase for large graphs).
    #[arg(long, env = "DIGILOCK_MAX_RESPONSE_BYTES", default_value_t = 8_388_608)]
    max_response_bytes: usize,
    /// Permit hardware writes, subject to any configured policy. Disabled by default.
    #[arg(long, env = "DIGILOCK_ALLOW_WRITES", default_value_t = false)]
    allow_writes: bool,
    /// JSON file containing write_policy and ordered initial_values.
    #[arg(long, env = "DIGILOCK_CONFIG")]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(args));
    // Tokio's stdin read runs in an uncancellable blocking task. Do not let an
    // MCP parent holding stdin open prevent process exit after Ctrl-C.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn run(args: Args) -> Result<()> {
    if args.transport == Transport::Network {
        // Transport configuration must fail before any startup hardware writes.
        network::validate_allowed_hosts(&args.allowed_host)?;
    }
    let server = DigiLockServer::new(Config {
        host: args.host,
        dms_port: args.dms_port,
        dui_port: args.dui_port,
        timeout: Duration::from_millis(args.timeout_ms),
        max_response_bytes: args.max_response_bytes,
        allow_writes: args.allow_writes,
        log_rci_commands: args.transport == Transport::Network,
        operator: args
            .config
            .as_deref()
            .map(config::OperatorConfig::load)
            .transpose()?,
    })?;
    // Bind before startup writes so an occupied/invalid listen address cannot
    // cause hardware changes for a server that will never become available.
    let listener = if args.transport == Transport::Network {
        Some(
            TcpListener::bind(args.listen)
                .await
                .context("binding MCP network listener")?,
        )
    } else {
        None
    };
    tokio::select! {
        result = server.initialize() => result.context("applying startup values; earlier writes may already have taken effect; no commands were replayed")?,
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for shutdown signal")?;
            anyhow::bail!("startup cancelled; in-flight or earlier writes may already have taken effect");
        }
    }
    if let Some(listener) = listener {
        return network::serve(listener, server, args.allowed_host).await;
    }
    serve_stdio(server).await
}

async fn serve_stdio(server: DigiLockServer) -> Result<()> {
    // Stdout belongs exclusively to the MCP transport; diagnostics go to stderr.
    let service = tokio::select! {
        result = server.serve(stdio()) => result.context("starting MCP stdio service")?,
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for shutdown signal")?;
            return Ok(());
        }
    };
    let cancellation = service.cancellation_token();
    let waiting = service.waiting();
    tokio::pin!(waiting);
    tokio::select! {
        result = &mut waiting => { result.context("running MCP stdio service")?; }
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for shutdown signal")?;
            cancellation.cancel();
            tokio::time::timeout(Duration::from_secs(2), &mut waiting)
                .await.context("timed out shutting down MCP service")?
                .context("shutting down MCP service")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_read_only() {
        let args = Args::try_parse_from(["digilock-mcp"]).unwrap();
        assert!(!args.allow_writes);
        assert_eq!(args.transport, Transport::Stdio);
        assert_eq!(args.listen, "127.0.0.1:8080".parse::<SocketAddr>().unwrap());
        assert_eq!(args.dms_port, 60000);
        assert_eq!(args.dui_port, 60001);
    }

    #[test]
    fn network_transport_and_listener_are_selectable() {
        let args = Args::try_parse_from([
            "digilock-mcp",
            "--transport",
            "network",
            "--listen",
            "0.0.0.0:9000",
            "--allowed-host",
            "mcp.example.test:9000",
        ])
        .unwrap();
        assert_eq!(args.transport, Transport::Network);
        assert_eq!(args.listen, "0.0.0.0:9000".parse::<SocketAddr>().unwrap());
        assert_eq!(args.allowed_host, ["mcp.example.test:9000"]);
        assert!(!args.allow_writes);
        assert!(Args::try_parse_from(["digilock-mcp", "--transport", "invalid"]).is_err());
        assert!(Args::try_parse_from(["digilock-mcp", "--listen", "invalid"]).is_err());
    }

    #[test]
    fn allowed_hosts_require_valid_authorities_and_ports() {
        let valid = [
            "localhost",
            "mcp.example.test:9000",
            "127.0.0.1",
            "::1",
            "[::1]",
            "[::1]:9000",
            "MCP.EXAMPLE.TEST:65535",
        ]
        .map(str::to_owned);
        network::validate_allowed_hosts(&valid).unwrap();
        for invalid in [
            "",
            " ",
            "mcp.test:99999",
            "mcp.test:abc",
            "mcp.test:",
            "user@mcp.test",
            "https://mcp.test",
            "*.test",
            "mcp.test/path",
            "mcp.test?query",
            "mcp.test#fragment",
            "mcp.test\r\n",
            " mcp.test",
        ] {
            assert!(
                network::validate_allowed_hosts(&[invalid.to_owned()]).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn explicit_write_opt_in() {
        assert!(
            Args::try_parse_from(["digilock-mcp", "--allow-writes"])
                .unwrap()
                .allow_writes
        );
    }
}
