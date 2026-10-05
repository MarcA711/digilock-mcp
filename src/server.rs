use std::{sync::Arc, time::Duration};

use anyhow::{Result, bail};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext, wrapper::Parameters},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation,
        ServerCapabilities, ServerConfig,
    },
    schemars::JsonSchema,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    catalog::{self, Command, Target},
    rci::RciClient,
};

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub dms_port: u16,
    pub dui_port: u16,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub allow_writes: bool,
}

#[derive(Clone)]
pub struct DigiLockServer {
    client: Arc<RciClient>,
    catalog: Arc<Vec<Command>>,
    config: Config,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Endpoint {
    /// dms = module management; dui = individual DigiLock controls.
    pub target: Target,
    /// Optional DUI port override. Obtain actual ports with DMS modules:port numbers?.
    /// For DMS this must be omitted or equal the configured DMS port.
    pub port: Option<u16>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ControlRequest {
    #[serde(flatten)]
    pub endpoint: Endpoint,
    /// Exact RCI control name (including spaces); no ?, =, .range?, or .help? suffix.
    pub command: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetRequest {
    #[serde(flatten)]
    pub control: ControlRequest,
    /// RCI value as a string: true/false, a numeric string, or an enumeration label.
    /// Preserve the Windows host's numeric decimal separator.
    pub value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RawRequest {
    #[serde(flatten)]
    pub endpoint: Endpoint,
    /// One complete RCI command, without CR/LF. Read-only mode permits only catalogued
    /// queries and their range/help queries. Other commands require --allow-writes.
    pub command: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CatalogRequest {
    /// Optional interface filter.
    pub target: Option<Target>,
    /// Case-insensitive substring to search in names and descriptions, e.g. pid1 or graph.
    pub filter: Option<String>,
    /// Return only settable controls when true; only read-only controls when false.
    pub writable: Option<bool>,
}

impl DigiLockServer {
    pub fn new(config: Config) -> Result<Self> {
        if config.host.trim().is_empty() {
            bail!("host must not be empty");
        }
        if config.dms_port == 0 || config.dui_port == 0 || config.dms_port == config.dui_port {
            bail!("DMS and DUI ports must be nonzero and distinct");
        }
        if config.timeout.is_zero() || config.max_response_bytes < 2 {
            bail!("timeout must be positive and max-response-bytes must be at least 2");
        }
        Ok(Self {
            client: Arc::new(RciClient::new(
                config.host.clone(),
                config.timeout,
                config.max_response_bytes,
            )),
            catalog: Arc::new(catalog::load()?),
            config,
            tool_router: Self::tool_router(),
        })
    }

    fn port(&self, endpoint: &Endpoint) -> Result<u16> {
        let port = endpoint.port.unwrap_or(match endpoint.target {
            Target::Dms => self.config.dms_port,
            Target::Dui => self.config.dui_port,
        });
        if port == 0 {
            bail!("port must not be zero");
        }
        match endpoint.target {
            Target::Dms if port != self.config.dms_port => {
                bail!("DMS port overrides must match the configured DMS port")
            }
            Target::Dui if port == self.config.dms_port => {
                bail!("DUI requests cannot use the DMS port")
            }
            _ => Ok(port),
        }
    }

    fn require_writes(&self) -> Result<()> {
        if !self.config.allow_writes {
            bail!(
                "hardware writes are disabled; restart the server with --allow-writes to enable them"
            );
        }
        Ok(())
    }

    fn query_command(&self, target: Target, name: &str, suffix: &str) -> Result<String> {
        catalog::validate_name(name)?;
        if suffix == "?" {
            match catalog::lookup(&self.catalog, target, name) {
                Some(entry) if !entry.query => bail!(
                    "{name} is set-only according to the manual; use range/help or digilock_set"
                ),
                None if !self.config.allow_writes => bail!(
                    "unknown command {name}; read-only mode permits only documented queries. Use digilock_query with commandlist to discover firmware commands; unknown commands require --allow-writes"
                ),
                _ => (),
            }
        } else if catalog::lookup(&self.catalog, target, name).is_none()
            && !self.config.allow_writes
        {
            bail!("unknown command {name}; read-only mode permits only documented controls");
        }
        Ok(format!("{name}{suffix}"))
    }

    fn set_command(&self, request: &SetRequest) -> Result<String> {
        self.require_writes()?;
        let name = &request.control.command;
        catalog::validate_name(name)?;
        catalog::validate_value(&request.value)?;
        if let Some(entry) = catalog::lookup(&self.catalog, request.control.endpoint.target, name) {
            if !entry.set {
                bail!("{name} is read-only according to the manual");
            }
            if entry.data_type == "boolean" && !matches!(request.value.as_str(), "true" | "false") {
                bail!("{name} requires the boolean string true or false");
            }
        }
        // Numeric ranges and enum labels are firmware/locale dependent; the RCI
        // validates them. The manual gives no portable limits to enforce here.
        Ok(format!("{name}={}", request.value))
    }

    fn validate_raw(&self, request: &RawRequest) -> Result<()> {
        let command = &request.command;
        if command.trim().is_empty() || command.chars().any(char::is_control) {
            bail!("raw command must be nonempty and contain no control characters");
        }
        if self.config.allow_writes {
            return Ok(());
        }
        for suffix in [".range?", ".help?", "?"] {
            if let Some(name) = command.strip_suffix(suffix) {
                self.query_command(request.endpoint.target, name, suffix)?;
                return Ok(());
            }
        }
        bail!("raw writes or unrecognized commands are disabled; use --allow-writes");
    }

    async fn execute(&self, endpoint: &Endpoint, command: &str) -> Result<Value> {
        let port = self.port(endpoint)?;
        let reply = self.client.execute(port, command).await?;
        Ok(
            json!({ "target": endpoint.target, "host": self.config.host, "port": port,
            "command": reply.command, "response": reply.response, "value": reply.value }),
        )
    }

    async fn control_query(&self, request: ControlRequest, suffix: &str) -> CallToolResult {
        let result = async {
            let command = self.query_command(request.endpoint.target, &request.command, suffix)?;
            self.execute(&request.endpoint, &command).await
        }
        .await;
        tool_result(result)
    }
}

fn tool_result(result: Result<Value>) -> CallToolResult {
    match result {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::structured_error(json!({"error": format!("{error:#}")})),
    }
}

#[tool_router]
impl DigiLockServer {
    #[tool(
        description = "Get configured DigiLock endpoints and safety settings without connecting to hardware.",
        annotations(read_only_hint = true)
    )]
    fn digilock_info(&self) -> CallToolResult {
        CallToolResult::structured(json!({
            "host": self.config.host,
            "dms_port": self.config.dms_port,
            "dui_port": self.config.dui_port,
            "timeout_ms": self.config.timeout.as_millis(),
            "max_response_bytes": self.config.max_response_bytes,
            "allow_writes": self.config.allow_writes,
            "manual": "DigiLock 110 RCI M-038 version 05, February 2023",
            "catalog_entries": self.catalog.len(),
        }))
    }

    #[tool(
        description = "Search the bundled manual command catalog offline. Lists exact RCI names, query/set access, data types, and descriptions. Firmware discovery: digilock_query with command commandlist.",
        annotations(read_only_hint = true)
    )]
    fn digilock_commands(&self, Parameters(request): Parameters<CatalogRequest>) -> CallToolResult {
        let filter = request.filter.unwrap_or_default().to_lowercase();
        let entries: Vec<_> = self
            .catalog
            .iter()
            .filter(|entry| {
                request.target.is_none_or(|target| entry.target == target)
                    && request
                        .writable
                        .is_none_or(|writable| entry.set == writable)
                    && (entry.command.to_lowercase().contains(&filter)
                        || entry.description.to_lowercase().contains(&filter))
            })
            .collect();
        CallToolResult::structured(json!({"count": entries.len(), "commands": entries}))
    }

    #[tool(
        description = "Query a DigiLock control (sends name?). For module discovery query DMS modules:names, modules:serial numbers, modules:port numbers, and modules:connection status. Returns raw text and an untyped string value; arrays/graphs are not parsed. Graph queries need the corresponding DUI display active.",
        annotations(read_only_hint = true)
    )]
    async fn digilock_query(
        &self,
        Parameters(request): Parameters<ControlRequest>,
    ) -> CallToolResult {
        self.control_query(request, "?").await
    }

    #[tool(
        description = "Set a DigiLock control (sends name=value). Requires operator --allow-writes. Can change physical hardware outputs, enable feedback, or stop applications; confirm the intended operation and query the range first. Values are strings using the Windows host's locale. Successful writes may return empty text; query again to verify.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn digilock_set(&self, Parameters(request): Parameters<SetRequest>) -> CallToolResult {
        tool_result(
            async {
                let command = self.set_command(&request)?;
                self.execute(&request.control.endpoint, &command).await
            }
            .await,
        )
    }

    #[tool(
        description = "Query allowed values and type with name.range?. Range/enum formatting is firmware dependent and is returned verbatim.",
        annotations(read_only_hint = true)
    )]
    async fn digilock_range(
        &self,
        Parameters(request): Parameters<ControlRequest>,
    ) -> CallToolResult {
        self.control_query(request, ".range?").await
    }

    #[tool(
        description = "Query firmware help for a control with name.help?.",
        annotations(read_only_hint = true)
    )]
    async fn digilock_help(
        &self,
        Parameters(request): Parameters<ControlRequest>,
    ) -> CallToolResult {
        self.control_query(request, ".help?").await
    }

    #[tool(
        description = "Send one raw RCI command without CR/LF. Escape hatch for firmware-specific names. Read-only mode permits only documented queries, range and help. With --allow-writes this can perform arbitrary hardware operations; never retry a timed-out write automatically.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false
        )
    )]
    async fn digilock_raw(&self, Parameters(request): Parameters<RawRequest>) -> CallToolResult {
        tool_result(
            async {
                self.validate_raw(&request)?;
                self.execute(&request.endpoint, &request.command).await
            }
            .await,
        )
    }

    #[tool(
        description = "Close this server's persistent connection to an endpoint, freeing its single RCI client slot. Does not disconnect the hardware module or change controls. A later query opens a new session.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn digilock_disconnect(
        &self,
        Parameters(endpoint): Parameters<Endpoint>,
    ) -> CallToolResult {
        tool_result(
            async {
                let port = self.port(&endpoint)?;
                let disconnected = self.client.disconnect(port).await;
                Ok(json!({"target": endpoint.target, "port": port, "disconnected": disconnected}))
            }
            .await,
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DigiLockServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let cancellation = context.ct.clone();
        let call = self
            .tool_router
            .call(ToolCallContext::new(self, request, context));
        // The SDK signals cancellation but does not drop tool futures itself.
        // Prefer cancellation before polling so queued writes cannot start later.
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Ok(CallToolResult::structured_error(json!({
                "error": "MCP request cancelled; no command was replayed. An in-flight write may already have taken effect."
            })).into()),
            result = call => result,
        }
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(format!(
                "Control TOPTICA DigiLock 110 through its Windows DMS/DUI software. Writes enabled: {}. Start with digilock_info and digilock_commands. DMS manages modules; DUI controls an individual module. Discover actual DUI ports using DMS modules:port numbers, not assumed module numbering. Each RCI endpoint allows one client: close other telnet/automation clients. Never automatically replay a timed-out write: it may already have executed. Query ranges before setting controls. For graphs activate the relevant display first (a write). Replies and values are strings; graph encoding is not specified by the manual.",
                self.config.allow_writes
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(allow_writes: bool) -> DigiLockServer {
        DigiLockServer::new(Config {
            host: "127.0.0.1".into(),
            dms_port: 60000,
            dui_port: 60001,
            timeout: Duration::from_secs(1),
            max_response_bytes: 1024,
            allow_writes,
        })
        .unwrap()
    }

    fn control(target: Target, command: &str) -> ControlRequest {
        ControlRequest {
            endpoint: Endpoint { target, port: None },
            command: command.into(),
        }
    }

    #[test]
    fn endpoint_selection_prevents_cross_interface_routing() {
        let server = server(false);
        assert_eq!(
            server.port(&control(Target::Dms, "").endpoint).unwrap(),
            60000
        );
        assert_eq!(
            server.port(&control(Target::Dui, "").endpoint).unwrap(),
            60001
        );
        assert_eq!(
            server
                .port(&Endpoint {
                    target: Target::Dui,
                    port: Some(60123)
                })
                .unwrap(),
            60123
        );
        for (target, port) in [(Target::Dms, 60001), (Target::Dui, 60000), (Target::Dui, 0)] {
            assert!(
                server
                    .port(&Endpoint {
                        target,
                        port: Some(port)
                    })
                    .is_err()
            );
        }
    }

    #[test]
    fn write_guard_and_manual_access_checks() {
        let mut request = SetRequest {
            control: control(Target::Dui, "pid1:lock:enable"),
            value: "true".into(),
        };
        assert!(server(false).set_command(&request).is_err());
        let server = server(true);
        assert_eq!(
            server.set_command(&request).unwrap(),
            "pid1:lock:enable=true"
        );
        request.value = "1".into();
        assert!(server.set_command(&request).is_err());
        request.control.command = "scope:graph".into();
        assert!(server.set_command(&request).is_err());
        request.control.command = "pid1:proportional".into();
        request.value = "0,25".into();
        assert_eq!(
            server.set_command(&request).unwrap(),
            "pid1:proportional=0,25"
        );
        request.value = "1\r\nprogram:exit=true".into();
        assert!(server.set_command(&request).is_err());
    }

    #[test]
    fn raw_cannot_bypass_read_only_mode() {
        let server = server(false);
        for command in [
            "pid1:lock:enable?",
            "pid1:lock:enable.range?",
            "pid1:lock:enable.help?",
            "echo.range?",
            "commandlist?",
        ] {
            server
                .validate_raw(&RawRequest {
                    endpoint: control(Target::Dui, "").endpoint,
                    command: command.into(),
                })
                .unwrap();
        }
        for command in [
            "pid1:lock:enable=true",
            "echo?",
            "unknown?",
            "pid1:lock:enable=true?",
            "pid1:lock:enable?\r\nprogram:exit=true",
            " pid1:lock:enable?",
            "pid1:lock:enable??",
        ] {
            assert!(
                server
                    .validate_raw(&RawRequest {
                        endpoint: control(Target::Dui, "").endpoint,
                        command: command.into()
                    })
                    .is_err(),
                "{command}"
            );
        }
    }

    #[test]
    fn query_and_metadata_syntax() {
        let server = server(false);
        assert_eq!(
            server
                .query_command(Target::Dui, "pid1:lock:enable", "?")
                .unwrap(),
            "pid1:lock:enable?"
        );
        assert!(
            server
                .query_command(Target::Dms, "pid1:lock:enable", "?")
                .is_err()
        );
        assert!(server.query_command(Target::Dui, "echo", "?").is_err());
        assert_eq!(
            server.query_command(Target::Dui, "echo", ".help?").unwrap(),
            "echo.help?"
        );
        assert!(
            server
                .query_command(Target::Dui, "pid1:lock:enable?", "?")
                .is_err()
        );
    }

    #[test]
    fn flattened_mcp_parameters_deserialize() {
        let request: SetRequest = serde_json::from_value(
            json!({"target": "dui", "port": 60002, "command": "pid1:lock:enable", "value": "true"}),
        )
        .unwrap();
        assert_eq!(request.control.endpoint.port, Some(60002));
        assert_eq!(request.control.command, "pid1:lock:enable");
        let schema = serde_json::to_value(rmcp::schemars::schema_for!(SetRequest)).unwrap();
        for field in ["target", "port", "command", "value"] {
            assert!(schema["properties"].get(field).is_some(), "{field}");
        }
    }

    #[tokio::test]
    async fn blocked_write_returns_tool_error_without_connecting() {
        let result = server(false)
            .digilock_set(Parameters(SetRequest {
                control: control(Target::Dui, "pid1:lock:enable"),
                value: "true".into(),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            result.structured_content.unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("writes are disabled")
        );
    }
}
