mod catalog;
mod config;
mod policy;
mod rci;
mod server;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};

use server::{Config, DigiLockServer};

/// MCP stdio server for the TOPTICA DigiLock 110 TCP Remote Control Interface.
/// The Windows DigiLock Module Server and the desired DUI must already be running.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
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
    let server = DigiLockServer::new(Config {
        host: args.host,
        dms_port: args.dms_port,
        dui_port: args.dui_port,
        timeout: Duration::from_millis(args.timeout_ms),
        max_response_bytes: args.max_response_bytes,
        allow_writes: args.allow_writes,
        operator: args
            .config
            .as_deref()
            .map(config::OperatorConfig::load)
            .transpose()?,
    })?;
    tokio::select! {
        result = server.initialize() => result.context("applying startup values; earlier writes may already have taken effect; no commands were replayed")?,
        result = tokio::signal::ctrl_c() => {
            result.context("waiting for shutdown signal")?;
            anyhow::bail!("startup cancelled; in-flight or earlier writes may already have taken effect");
        }
    }
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
        assert_eq!(args.dms_port, 60000);
        assert_eq!(args.dui_port, 60001);
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
