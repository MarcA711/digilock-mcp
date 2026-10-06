//! End-to-end tests of the shipped binary, using MCP JSON-RPC over stdio and
//! fake local RCI servers. No DigiLock hardware or Python dependencies required.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::oneshot,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    async fn start(extra: &[String]) -> Self {
        let mut client = Self::spawn(binary(extra));
        client.initialize().await;
        client
    }

    fn spawn(mut command: Command) -> Self {
        let mut child = command.spawn().unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 1,
        }
    }

    async fn initialize(&mut self) {
        let init = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26", "capabilities": {},
                    "clientInfo": {"name": "integration-test", "version": "1"}
                }),
            )
            .await;
        assert!(init.get("result").is_some(), "{init}");
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        self.input.write_all(&bytes).await.unwrap();
        self.input.flush().await.unwrap();
    }

    async fn begin(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        let count = timeout(DEADLINE, self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(count > 0, "MCP server closed stdout");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.begin(method, params).await;
        let reply = self.receive().await;
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let reply = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }

    async fn stop(self) {
        let Self {
            mut child,
            input,
            output,
            ..
        } = self;
        drop(input);
        drop(output);
        let status = timeout(DEADLINE, child.wait()).await.unwrap().unwrap();
        assert!(status.success(), "{status}");
    }
}

fn binary(extra: &[String]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_digilock-mcp"));
    command
        .args(["--host", "127.0.0.1"])
        .args(extra)
        .env_remove("DIGILOCK_CONFIG")
        .env_remove("DIGILOCK_ALLOW_WRITES")
        .env_remove("DIGILOCK_DMS_PORT")
        .env_remove("DIGILOCK_DUI_PORT")
        .env_remove("DIGILOCK_TIMEOUT_MS")
        .env_remove("DIGILOCK_MAX_RESPONSE_BYTES")
        .env_remove("DIGILOCK_TRANSPORT")
        .env_remove("DIGILOCK_LISTEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command
}

struct ConfigFile(PathBuf);

impl ConfigFile {
    fn new(config: Value) -> Self {
        Self::raw(&serde_json::to_string(&config).unwrap())
    }

    fn raw(contents: &str) -> Self {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "digilock-mcp-stdio-{}-{}.json",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let config = Self(path);
                    file.write_all(contents.as_bytes()).unwrap();
                    return config;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating temporary configuration: {error}"),
            }
        }
    }

    fn args(&self) -> [String; 2] {
        ["--config".into(), self.0.to_str().unwrap().into()]
    }
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn accept_rci(listener: &TcpListener) -> BufReader<TcpStream> {
    let (mut stream, _) = timeout(DEADLINE, listener.accept()).await.unwrap().unwrap();
    stream.write_all(b"Fake RCI\r\n> ").await.unwrap();
    BufReader::new(stream)
}

async fn expect_rci_command(stream: &mut BufReader<TcpStream>, expected: &str) {
    let mut command = String::new();
    timeout(DEADLINE, stream.read_line(&mut command))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command, format!("{expected}\r\n"));
}

async fn expect_rci_closed(stream: &mut BufReader<TcpStream>) {
    let mut command = String::new();
    let count = timeout(DEADLINE, stream.read_line(&mut command))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count, 0, "unexpected additional RCI command: {command:?}");
}

async fn scripted_rci(listener: TcpListener, exchanges: &[(&str, &str)]) {
    let mut stream = accept_rci(&listener).await;
    for (expected, reply) in exchanges {
        expect_rci_command(&mut stream, expected).await;
        stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
    }
    expect_rci_closed(&mut stream).await;
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "unexpected RCI reconnection"
    );
}

async fn assert_no_connection(listener: &TcpListener) {
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "invalid or denied operation opened a TCP connection"
    );
}

async fn assert_startup_failure(mut command: Command) -> String {
    command.stderr(Stdio::piped());
    let output = timeout(DEADLINE, command.spawn().unwrap().wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success(), "startup unexpectedly succeeded");
    assert!(
        output.stdout.is_empty(),
        "startup emitted MCP stdout: {output:?}"
    );
    String::from_utf8(output.stderr).unwrap()
}

async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

#[tokio::test]
async fn initializes_lists_tools_and_enforces_read_only_without_hardware() {
    let mut mcp = Mcp::start(&[]).await;
    let tools = mcp.request("tools/list", json!({})).await;
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    let setter = tools.iter().find(|t| t["name"] == "digilock_set").unwrap();
    assert_eq!(setter["annotations"]["destructiveHint"], true);
    for field in ["target", "command", "value"] {
        assert!(setter["inputSchema"]["properties"].get(field).is_some());
    }
    let info = mcp.call("digilock_info", json!({})).await;
    assert_eq!(info["structuredContent"]["allow_writes"], false);
    let catalog = mcp
        .call(
            "digilock_commands",
            json!({"target": "dui", "filter": "scope:graph"}),
        )
        .await;
    assert_eq!(catalog["structuredContent"]["count"], 1);
    for (tool, args) in [
        (
            "digilock_set",
            json!({"target": "dui", "command": "pid1:lock:enable", "value": "true"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:lock:enable=true"}),
        ),
        (
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional?\r\nprogram:exit=true"}),
        ),
    ] {
        let result = mcp.call(tool, args).await;
        assert_eq!(result["isError"], true, "{result}");
    }
    mcp.stop().await;
}

#[tokio::test]
async fn query_range_device_error_and_disconnect_round_trip() {
    let (listener, port) = listener().await;
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"DigiLock welcome\r\n> ").await.unwrap();
        let mut stream = BufReader::new(stream);
        for (expected, reply) in [
            (
                "pid1:proportional?",
                "pid1:proportional?\r\npid1:proportional=1,25\r\n> ",
            ),
            (
                "pid1:proportional.range?",
                "pid1:proportional.range=0 ... 10000\r\n> ",
            ),
            (
                "pid1:proportional.help?",
                "%% Error: bad command-pid1:proportional.help?\r\n> ",
            ),
            ("scope:graph?", "scope:graph=1;2\r\n3;4\r\n> "),
        ] {
            let mut command = String::new();
            stream.read_line(&mut command).await.unwrap();
            assert_eq!(command, format!("{expected}\r\n"));
            stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let mut mcp = Mcp::start(&["--dui-port".into(), port.to_string()]).await;
    let query = mcp
        .call(
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(query["structuredContent"]["value"], "1,25");
    assert_eq!(query["structuredContent"]["port"], port);
    let range = mcp
        .call(
            "digilock_range",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(range["structuredContent"]["value"], "0 ... 10000");
    let error = mcp
        .call(
            "digilock_help",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(error["isError"], true);
    assert!(
        error["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("%% Error:")
    );
    let graph = mcp
        .call(
            "digilock_query",
            json!({"target": "dui", "command": "scope:graph"}),
        )
        .await;
    assert_eq!(graph["structuredContent"]["value"], "1;2\r\n3;4");
    let disconnect = mcp
        .call("digilock_disconnect", json!({"target": "dui"}))
        .await;
    assert_eq!(disconnect["structuredContent"]["disconnected"], true);
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    mcp.stop().await;
}

#[tokio::test]
async fn opt_in_writes_route_to_dms_and_override_dui_port() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let dms_task = tokio::spawn(async move {
        let (mut stream, _) = dms.accept().await.unwrap();
        stream.write_all(b"DMS\r\n> ").await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut command = String::new();
        stream.read_line(&mut command).await.unwrap();
        assert_eq!(command, "selected module=2\r\n");
        stream.get_mut().write_all(b"> ").await.unwrap();
    });
    let dui_task = tokio::spawn(async move {
        let (mut stream, _) = dui.accept().await.unwrap();
        stream.write_all(b"DUI\r\n> ").await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut command = String::new();
        stream.read_line(&mut command).await.unwrap();
        assert_eq!(command, "pid1:proportional=0,25\r\n");
        stream.get_mut().write_all(b"> ").await.unwrap();
    });
    let mut mcp = Mcp::start(&[
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
    ])
    .await;
    let result = mcp
        .call(
            "digilock_set",
            json!({"target": "dms", "command": "selected module", "value": "2"}),
        )
        .await;
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["response"], "");
    let result = mcp.call("digilock_set", json!({"target": "dui", "port": dui_port, "command": "pid1:proportional", "value": "0,25"})).await;
    assert_eq!(result["isError"], false);
    timeout(DEADLINE, dms_task).await.unwrap().unwrap();
    timeout(DEADLINE, dui_task).await.unwrap().unwrap();
    mcp.stop().await;
}

#[tokio::test]
async fn mcp_cancellation_closes_in_flight_session_and_prevents_queued_write() {
    let (listener, port) = listener().await;
    let (started_tx, started_rx) = oneshot::channel();
    let (check_tx, check_rx) = oneshot::channel();
    let (second_tx, second_rx) = oneshot::channel();
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.write_all(b"DUI\r\n> ").await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut command = String::new();
        stream.read_line(&mut command).await.unwrap();
        assert_eq!(command, "pid1:proportional?\r\n");
        started_tx.send(()).unwrap();
        check_rx.await.unwrap();
        stream
            .get_mut()
            .write_all(b"pid1:proportional=1\r\n> ")
            .await
            .unwrap();
        command.clear();
        stream.read_line(&mut command).await.unwrap();
        assert_eq!(
            command, "pid1:proportional?\r\n",
            "cancelled queued write was sent"
        );
        second_tx.send(()).unwrap();
        // The second query will be cancelled instead of receiving a reply.
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let mut mcp = Mcp::start(&[
        "--allow-writes".into(),
        "--dui-port".into(),
        port.to_string(),
    ])
    .await;
    let first = mcp.begin("tools/call", json!({"name": "digilock_query", "arguments": {"target": "dui", "command": "pid1:proportional"}})).await;
    timeout(DEADLINE, started_rx).await.unwrap().unwrap();
    let queued = mcp.begin("tools/call", json!({"name": "digilock_set", "arguments": {"target": "dui", "command": "pid1:proportional", "value": "999"}})).await;
    mcp.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": queued, "reason": "test queued cancellation"}})).await;
    // MCP suppresses responses for cancelled requests. An offline call is a
    // barrier confirming the notification has been processed before releasing RCI.
    assert_eq!(mcp.call("digilock_info", json!({})).await["isError"], false);
    check_tx.send(()).unwrap();
    let completed = mcp.receive().await;
    assert_eq!(completed["id"], first);
    assert_eq!(completed["result"]["isError"], false);
    let second = mcp.begin("tools/call", json!({"name": "digilock_query", "arguments": {"target": "dui", "command": "pid1:proportional"}})).await;
    timeout(DEADLINE, second_rx).await.unwrap().unwrap();
    mcp.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": second, "reason": "test in-flight cancellation"}})).await;
    assert_eq!(mcp.call("digilock_info", json!({})).await["isError"], false);
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    mcp.stop().await;
}

#[tokio::test]
async fn config_allowlist_applies_to_sets_and_raw_without_restricting_queries() {
    let (dms, dms_port) = listener().await;
    let (dui, port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {
            "mode": "allowlist",
            "commands": [
                {"target": "dui", "command": "pid1:proportional"},
                {"target": "dui", "command": "pid1:lock:enable"}
            ],
            "limits": [
                {"target": "dui", "command": "pid1:proportional", "min": 0, "max": 1},
                {"target": "dui", "command": "pid1:lock:enable", "allowed_values": ["false"]}
            ]
        }
    }));
    let fake = tokio::spawn(async move {
        scripted_rci(
            dui,
            &[
                ("pid1:proportional=0,25", "> "),
                ("pid1:proportional=1e0", "> "),
                ("pid1:lock:enable=false", "> "),
                ("pid1:proportional?", "pid1:proportional=999\r\n> "),
                ("pid1:lock:enable?", "pid1:lock:enable=true\r\n> "),
                ("pid2:proportional?", "pid2:proportional=999\r\n> "),
                (
                    "pid2:proportional.range?",
                    "pid2:proportional.range=0 ... 10000\r\n> ",
                ),
                ("pid2:proportional.help?", "help text\r\n> "),
                ("pid2:proportional?", "pid2:proportional=999\r\n> "),
                ("firmware:extra?", "firmware:extra=42\r\n> "),
            ],
        )
        .await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let mut mcp = Mcp::start(&args).await;
    for (tool, arguments) in [
        (
            "digilock_set",
            json!({"target": "dui", "command": "pid2:proportional", "value": "0.25"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid2:proportional=0.25"}),
        ),
        (
            "digilock_set",
            json!({"target": "dms", "command": "pid1:proportional", "value": "0.25"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dms", "command": "pid1:proportional=0.25"}),
        ),
        (
            "digilock_set",
            json!({"target": "dui", "command": "pid1:lock:enable", "value": "true"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:lock:enable=true"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:lock:enable=0"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "scope:graph=1"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "program:exit"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:proportional=0.25;program:exit=true"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:proportional?\r\nprogram:exit=true"}),
        ),
    ] {
        let result = mcp.call(tool, arguments).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
    }
    for value in ["-0.01", "1.01", "NaN", "inf", "0,2.5", " 0.25"] {
        for (tool, arguments) in [
            (
                "digilock_set",
                json!({"target": "dui", "command": "pid1:proportional", "value": value}),
            ),
            (
                "digilock_raw",
                json!({"target": "dui", "command": format!("pid1:proportional={value}")}),
            ),
        ] {
            let result = mcp.call(tool, arguments).await;
            assert_eq!(
                result["isError"], true,
                "{tool} accepted {value:?}: {result}"
            );
        }
    }
    for (tool, arguments) in [
        (
            "digilock_set",
            json!({"target": "dui", "command": "pid1:proportional", "value": "0,25"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:proportional=1e0"}),
        ),
        (
            "digilock_set",
            json!({"target": "dui", "command": "pid1:lock:enable", "value": "false"}),
        ),
        (
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:lock:enable?"}),
        ),
        (
            "digilock_query",
            json!({"target": "dui", "command": "pid2:proportional"}),
        ),
        (
            "digilock_range",
            json!({"target": "dui", "command": "pid2:proportional"}),
        ),
        (
            "digilock_help",
            json!({"target": "dui", "command": "pid2:proportional"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid2:proportional?"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "firmware:extra?"}),
        ),
    ] {
        let result = mcp.call(tool, arguments).await;
        assert_eq!(result["isError"], false, "{tool}: {result}");
    }
    mcp.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    assert_no_connection(&dms).await;
}

#[tokio::test]
async fn config_denylist_and_limits_cannot_be_bypassed_by_raw_aliases_or_ports() {
    let (dms, dms_port) = listener().await;
    let (dui, port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {
            "mode": "denylist",
            "commands": [{"target": "dui", "command": "pid1:lock:enable"}],
            "limits": [{"target": "dui", "command": "pid1:proportional", "max": 1,
                        "allowed_values": ["0,25", "1e0", "2"]}]
        }
    }));
    let fake = tokio::spawn(async move {
        scripted_rci(
            dui,
            &[
                ("pid1:proportional=0,25", "> "),
                ("PID1:PROPORTIONAL=1e0", "> "),
                ("pid1:lock:enable?", "pid1:lock:enable=true\r\n> "),
                ("pid1:lock:enable?", "pid1:lock:enable=true\r\n> "),
            ],
        )
        .await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let mut mcp = Mcp::start(&args).await;
    for command in ["pid1:lock:enable", "PID1:LOCK:ENABLE", "pid1: lock: enable"] {
        for (tool, arguments) in [
            (
                "digilock_set",
                json!({"target": "dui", "port": other_port, "command": command, "value": "true"}),
            ),
            (
                "digilock_raw",
                json!({"target": "dui", "port": other_port, "command": format!("{command}=true")}),
            ),
        ] {
            let result = mcp.call(tool, arguments).await;
            assert_eq!(result["isError"], true, "{tool}: {result}");
            assert!(
                result["structuredContent"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("denylisted"),
                "{result}"
            );
        }
    }
    for value in ["0.25", "2"] {
        for (tool, arguments) in [
            (
                "digilock_set",
                json!({"target": "dui", "port": other_port, "command": "PID1:PROPORTIONAL", "value": value}),
            ),
            (
                "digilock_raw",
                json!({"target": "dui", "port": other_port, "command": format!("pid1: proportional={value}")}),
            ),
        ] {
            let result = mcp.call(tool, arguments).await;
            assert_eq!(result["isError"], true, "{tool}: {result}");
            let error = result["structuredContent"]["error"].as_str().unwrap();
            assert!(
                error.contains(if value == "2" {
                    "maximum"
                } else {
                    "not allowed"
                }),
                "{result}"
            );
        }
    }
    assert_no_connection(&other_dui).await;
    for (tool, arguments) in [
        (
            "digilock_set",
            json!({"target": "dui", "command": "pid1:proportional", "value": "0,25"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "PID1:PROPORTIONAL=1e0"}),
        ),
        (
            "digilock_query",
            json!({"target": "dui", "command": "pid1:lock:enable"}),
        ),
        (
            "digilock_raw",
            json!({"target": "dui", "command": "pid1:lock:enable?"}),
        ),
    ] {
        let result = mcp.call(tool, arguments).await;
        assert_eq!(result["isError"], false, "{tool}: {result}");
    }
    mcp.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    assert_no_connection(&dms).await;
}

#[tokio::test]
async fn shipped_preset_rejections_never_connect_for_sets_raw_aliases_or_port_overrides() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let preset: Value =
        serde_json::from_str(include_str!("../digilock-config.ta-pro-lock-in.json")).unwrap();
    let config = ConfigFile::new(preset.clone());
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        dui_port.to_string(),
    ]);
    let mut mcp = Mcp::start(&args).await;
    let info = mcp.call("digilock_info", json!({})).await;
    let loaded = &info["structuredContent"]["write_policy"];
    for field in ["mode", "commands"] {
        assert_eq!(loaded[field], preset["write_policy"][field]);
    }
    for field in ["min", "max"] {
        assert_eq!(
            loaded["scan_range"][field].as_f64(),
            preset["write_policy"]["scan_range"][field].as_f64()
        );
    }
    let actual_limits = loaded["limits"].as_array().unwrap();
    let expected_limits = preset["write_policy"]["limits"].as_array().unwrap();
    assert_eq!(actual_limits.len(), expected_limits.len());
    for (actual, expected) in actual_limits.iter().zip(expected_limits) {
        for field in ["target", "command", "allowed_values"] {
            assert_eq!(actual[field], expected[field]);
        }
        for field in ["min", "max"] {
            assert_eq!(actual[field].as_f64(), expected[field].as_f64());
        }
    }

    let catalog: Vec<Value> = serde_json::from_str(include_str!("../docs/commands.json")).unwrap();
    let mut rejected = Vec::new();
    for entry in catalog.iter().filter(|entry| entry["set"] == true) {
        let target = entry["target"].as_str().unwrap();
        let name = entry["command"].as_str().unwrap();
        let listed = preset["write_policy"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|key| key["target"] == target && key["command"] == name);
        if !listed {
            let value = if entry["data_type"] == "boolean" {
                "true"
            } else {
                "0"
            };
            rejected.push((target, name.to_owned(), value.to_owned()));
        }
    }
    for limit in preset["write_policy"]["limits"].as_array().unwrap() {
        let name = limit["command"].as_str().unwrap();
        for value in [
            (limit["min"].as_f64().unwrap() - 1.0).to_string(),
            (limit["max"].as_f64().unwrap() + 1.0).to_string(),
            "NaN".into(),
            "inf".into(),
            "-inf".into(),
        ] {
            rejected.push(("dui", name.to_owned(), value));
        }
    }
    for (target, name, value) in rejected {
        for alias in [name.clone(), name.replace(' ', "").to_ascii_uppercase()] {
            let port = if target == "dms" {
                dms_port
            } else {
                other_port
            };
            for (tool, arguments) in [
                (
                    "digilock_set",
                    json!({"target": target, "port": port, "command": alias, "value": value}),
                ),
                (
                    "digilock_raw",
                    json!({"target": target, "port": port, "command": format!("{alias}={value}")}),
                ),
            ] {
                let result = mcp.call(tool, arguments).await;
                assert_eq!(
                    result["isError"], true,
                    "{tool} accepted {target}:{alias}={value}: {result}"
                );
            }
        }
    }
    mcp.stop().await;
    assert_no_connection(&dms).await;
    assert_no_connection(&dui).await;
    assert_no_connection(&other_dui).await;
}

#[tokio::test]
async fn scan_range_setters_use_fresh_counterparts_and_inclusive_peak_to_peak_amplitude() {
    let (dui, port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist", "scan_range": {"min": -2, "max": 6}}
    }));
    let fake = tokio::spawn(async move {
        scripted_rci(
            dui,
            &[
                // Peak-to-peak amplitude 8 at offset 2 touches both bounds: [-2, 6].
                ("scan:amplitude?", "scan:amplitude=8\r\n> "),
                ("offset:value=2", "> "),
                ("offset:value?", "offset:value=2\r\n> "),
                ("scan:amplitude=8", "> "),
                // Proposed values just outside either bound must not be written.
                ("scan:amplitude?", "scan:amplitude=8\r\n> "),
                ("scan:amplitude?", "scan:amplitude=8\r\n> "),
                ("offset:value?", "offset:value=2\r\n> "),
                // Simulate changes made outside the wrapper, without intervening writes.
                ("scan:amplitude?", "scan:amplitude=8.02\r\n> "),
                ("scan:amplitude?", "scan:amplitude=7.98\r\n> "),
                ("offset:value=2", "> "),
                ("offset:value?", "offset:value=2.01\r\n> "),
                ("offset:value?", "offset:value=1.99\r\n> "),
                ("offset:value?", "offset:value=2\r\n> "),
                ("scan:amplitude=8", "> "),
            ],
        )
        .await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let mut mcp = Mcp::start(&args).await;
    for (command, value, denied) in [
        ("offset:value", "2", false),
        ("scan:amplitude", "8", false),
        ("offset:value", "2.01", true),
        ("offset:value", "1.99", true),
        ("scan:amplitude", "8.02", true),
        ("offset:value", "2", true),
        ("offset:value", "2", false),
        ("scan:amplitude", "8", true),
        ("scan:amplitude", "8", true),
        ("scan:amplitude", "8", false),
    ] {
        let result = mcp
            .call(
                "digilock_set",
                json!({"target": "dui", "command": command, "value": value}),
            )
            .await;
        assert_eq!(result["isError"], denied, "{command}={value}: {result}");
        if !denied {
            assert_eq!(
                result["structuredContent"]["command"],
                format!("{command}={value}")
            );
            assert_eq!(result["structuredContent"]["response"], "");
            assert_eq!(result["structuredContent"]["port"], port);
        }
    }
    mcp.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
}

#[tokio::test]
async fn scan_range_raw_aliases_are_guarded_on_each_actual_dui_endpoint_only() {
    let (dms, dms_port) = listener().await;
    let (dui, port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist", "scan_range": {"min": -2, "max": 6}}
    }));
    let dms_fake = tokio::spawn(async move {
        // The same spelling on DMS is not a protected DUI control.
        scripted_rci(dms, &[("SCAN: AMPLITUDE=-1", "> ")]).await;
    });
    let fake = tokio::spawn(async move {
        scripted_rci(
            dui,
            &[
                ("scan:amplitude?", "scan:amplitude=8\r\n> "),
                ("OFFSET: VALUE=2", "> "),
                ("offset:value?", "offset:value=99\r\n> "),
                ("pid1:proportional=0.25", "> "),
            ],
        )
        .await;
    });
    let other_fake = tokio::spawn(async move {
        scripted_rci(
            other_dui,
            &[
                ("scan:amplitude?", "scan:amplitude=10\r\n> "),
                ("scan:amplitude?", "scan:amplitude=8\r\n> "),
                ("oFfSeT: vAlUe=2e0", "> "),
                ("offset:value?", "offset:value=2\r\n> "),
                ("ScAn: AmPlItUdE=8,0", "> "),
                ("offset:value?", "offset:value=3\r\n> "),
            ],
        )
        .await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let mut mcp = Mcp::start(&args).await;
    for (arguments, denied) in [
        (
            json!({"target": "dms", "command": "SCAN: AMPLITUDE=-1"}),
            false,
        ),
        (
            json!({"target": "dui", "command": "OFFSET: VALUE=2"}),
            false,
        ),
        (json!({"target": "dui", "command": "offset:value?"}), false),
        (
            json!({"target": "dui", "command": "pid1:proportional=0.25"}),
            false,
        ),
        (
            json!({"target": "dui", "port": other_port, "command": "oFfSeT: vAlUe=2e0"}),
            true,
        ),
        (
            json!({"target": "dui", "port": other_port, "command": "oFfSeT: vAlUe=2e0"}),
            false,
        ),
        (
            json!({"target": "dui", "port": other_port, "command": "ScAn: AmPlItUdE=8,0"}),
            false,
        ),
        (
            json!({"target": "dui", "port": other_port, "command": "ScAn: AmPlItUdE=8,0"}),
            true,
        ),
    ] {
        let result = mcp.call("digilock_raw", arguments.clone()).await;
        assert_eq!(result["isError"], denied, "{arguments}: {result}");
        if !denied {
            let expected_port = if arguments["target"] == "dms" {
                dms_port
            } else {
                arguments["port"].as_u64().map_or(port, |port| port as u16)
            };
            assert_eq!(result["structuredContent"]["port"], expected_port);
            assert_eq!(result["structuredContent"]["command"], arguments["command"]);
        }
    }
    mcp.stop().await;
    for task in [dms_fake, fake, other_fake] {
        timeout(DEADLINE, task).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn scan_range_optional_bounds_and_absent_default_preserve_expected_behavior() {
    for (range, offset, amplitude, denied) in [
        (Some(json!({"min": -2})), "100", "204", false),
        (Some(json!({"min": -2})), "100", "204.02", true),
        (Some(json!({"max": 6})), "-100", "212", false),
        (Some(json!({"max": 6})), "-100", "212.02", true),
        (Some(json!({"min": 2, "max": 2})), "2", "0", false),
        (None, "999", "-1", false),
    ] {
        let (dui, port) = listener().await;
        let mut policy = json!({"mode": "denylist"});
        if let Some(range) = &range {
            policy["scan_range"] = range.clone();
        }
        let config = ConfigFile::new(json!({"write_policy": policy}));
        let protected = range.is_some();
        let fake = tokio::spawn(async move {
            let mut stream = accept_rci(&dui).await;
            if protected {
                expect_rci_command(&mut stream, "offset:value?").await;
                stream
                    .get_mut()
                    .write_all(format!("offset:value={offset}\r\n> ").as_bytes())
                    .await
                    .unwrap();
            }
            if !denied {
                expect_rci_command(&mut stream, &format!("scan:amplitude={amplitude}")).await;
                stream.get_mut().write_all(b"> ").await.unwrap();
            }
            if !protected {
                // Absent scan_range leaves both setters free of counterpart queries.
                expect_rci_command(&mut stream, &format!("offset:value={offset}")).await;
                stream.get_mut().write_all(b"> ").await.unwrap();
            }
            expect_rci_closed(&mut stream).await;
            assert_no_connection(&dui).await;
        });
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dui-port".into(),
            port.to_string(),
        ]);
        let mut mcp = Mcp::start(&args).await;
        let result = mcp
            .call(
                "digilock_set",
                json!({
                    "target": "dui", "command": "scan:amplitude", "value": amplitude
                }),
            )
            .await;
        assert_eq!(result["isError"], denied, "{range:?}: {result}");
        if !protected {
            let result = mcp
                .call(
                    "digilock_raw",
                    json!({
                        "target": "dui", "command": format!("offset:value={offset}")
                    }),
                )
                .await;
            assert_eq!(result["isError"], false, "{result}");
        }
        mcp.stop().await;
        timeout(DEADLINE, fake).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn scan_range_static_rejections_preserve_write_guards_without_querying() {
    for (mode, commands, limits, allow_writes, rejected) in [
        (
            "allowlist",
            json!([]),
            json!([]),
            true,
            vec![("OFFSET: VALUE", "2"), ("SCAN: AMPLITUDE", "4")],
        ),
        (
            "denylist",
            json!([
                {"target": "dui", "command": "offset:value"},
                {"target": "dui", "command": "scan:amplitude"}
            ]),
            json!([]),
            true,
            vec![("OFFSET: VALUE", "2"), ("SCAN: AMPLITUDE", "4")],
        ),
        (
            "denylist",
            json!([]),
            json!([
                {"target": "dui", "command": "offset:value", "max": 1},
                {"target": "dui", "command": "scan:amplitude", "allowed_values": ["0"]}
            ]),
            true,
            vec![("OFFSET: VALUE", "2"), ("SCAN: AMPLITUDE", "4")],
        ),
        (
            "denylist",
            json!([]),
            json!([]),
            false,
            vec![("offset:value", "2"), ("scan:amplitude", "4")],
        ),
        (
            "denylist",
            json!([]),
            json!([]),
            true,
            vec![
                ("offset:value", "NaN"),
                ("offset:value", "inf"),
                ("offset:value", "not-a-number"),
                ("scan: amplitude", "-0.01"),
                ("SCAN:AMPLITUDE", "NaN"),
                ("scan:amplitude", "-inf"),
                ("scan:amplitude", "1e999"),
                ("scan:amplitude", "not-a-number"),
            ],
        ),
    ] {
        let (dui, port) = listener().await;
        let config = ConfigFile::new(json!({"write_policy": {
            "mode": mode, "commands": commands, "limits": limits,
            "scan_range": {"min": -2, "max": 6}
        }}));
        let mut args = config.args().to_vec();
        args.extend(["--dui-port".into(), port.to_string()]);
        if allow_writes {
            args.push("--allow-writes".into());
        }
        let mut mcp = Mcp::start(&args).await;
        for (command, value) in rejected {
            for (tool, arguments) in [
                (
                    "digilock_set",
                    json!({"target": "dui", "command": command, "value": value}),
                ),
                (
                    "digilock_raw",
                    json!({"target": "dui", "command": format!("{command}={value}")}),
                ),
            ] {
                let result = mcp.call(tool, arguments).await;
                assert_eq!(
                    result["isError"], true,
                    "{mode}, {tool}, {command}={value}: {result}"
                );
            }
        }
        assert_no_connection(&dui).await;
        mcp.stop().await;
    }
}

#[tokio::test]
async fn scan_range_bad_current_values_and_device_errors_never_send_a_write() {
    for (command, counterpart, bad_values) in [
        (
            "offset:value",
            "scan:amplitude",
            vec!["bad", "NaN", "inf", "-0.01", "1e999"],
        ),
        (
            "scan:amplitude",
            "offset:value",
            vec!["bad", "NaN", "-inf", "1e999"],
        ),
    ] {
        let (dui, port) = listener().await;
        let config = ConfigFile::new(json!({
            "write_policy": {"mode": "denylist", "scan_range": {"min": -2, "max": 6}}
        }));
        let mut replies = vec![
            "%% Error: cannot read current control\r\n> ".to_owned(),
            "> ".to_owned(),
            "unrelated:value=0\r\n> ".to_owned(),
            format!("{counterpart}=\r\n> "),
        ];
        replies.extend(
            bad_values
                .iter()
                .map(|value| format!("{counterpart}={value}\r\n> ")),
        );
        let count = replies.len();
        let fake = tokio::spawn(async move {
            let mut stream = accept_rci(&dui).await;
            for reply in replies {
                for _ in 0..2 {
                    expect_rci_command(&mut stream, &format!("{counterpart}?")).await;
                    stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
                }
            }
            // A safe raw assignment still propagates the actual write's device error.
            expect_rci_command(&mut stream, &format!("{counterpart}?")).await;
            let current = if counterpart == "scan:amplitude" {
                "4"
            } else {
                "2"
            };
            stream
                .get_mut()
                .write_all(format!("{counterpart}={current}\r\n> ").as_bytes())
                .await
                .unwrap();
            let proposed = if command == "offset:value" { "2" } else { "4" };
            expect_rci_command(&mut stream, &format!("{command}={proposed}")).await;
            stream
                .get_mut()
                .write_all(b"%% Error: rejected write\r\n> ")
                .await
                .unwrap();
            expect_rci_closed(&mut stream).await;
            assert_no_connection(&dui).await;
        });
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dui-port".into(),
            port.to_string(),
        ]);
        let mut mcp = Mcp::start(&args).await;
        let value = if command == "offset:value" { "2" } else { "4" };
        for index in 0..count {
            for (tool, arguments) in [
                (
                    "digilock_set",
                    json!({"target": "dui", "command": command, "value": value}),
                ),
                (
                    "digilock_raw",
                    json!({"target": "dui", "command": format!("{command}={value}")}),
                ),
            ] {
                let result = mcp.call(tool, arguments).await;
                assert_eq!(
                    result["isError"], true,
                    "{command}, reply {index}, {tool}: {result}"
                );
                assert!(
                    result["structuredContent"]["error"].as_str().is_some(),
                    "{result}"
                );
            }
        }
        let result = mcp
            .call(
                "digilock_raw",
                json!({
                    "target": "dui", "command": format!("{command}={value}")
                }),
            )
            .await;
        assert_eq!(result["isError"], true, "{result}");
        assert!(
            result["structuredContent"]["error"]
                .as_str()
                .unwrap()
                .contains("rejected write"),
            "{result}"
        );
        mcp.stop().await;
        timeout(DEADLINE, fake).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn scan_range_failed_counterpart_query_closes_without_assignment_or_retry() {
    for (tool, command, counterpart) in [
        ("digilock_set", "offset:value", "scan:amplitude"),
        ("digilock_raw", "scan:amplitude", "offset:value"),
    ] {
        let (dui, port) = listener().await;
        let config = ConfigFile::new(json!({
            "write_policy": {"mode": "denylist", "scan_range": {"max": 6}}
        }));
        let fake = tokio::spawn(async move {
            let mut stream = accept_rci(&dui).await;
            expect_rci_command(&mut stream, &format!("{counterpart}?")).await;
            // EOF before a complete query reply must not lead to an assignment.
            stream
                .get_mut()
                .write_all(format!("{counterpart}=0\r\n>").as_bytes())
                .await
                .unwrap();
            stream.get_mut().shutdown().await.unwrap();
            expect_rci_closed(&mut stream).await;
            assert_no_connection(&dui).await;
        });
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dui-port".into(),
            port.to_string(),
        ]);
        let mut mcp = Mcp::start(&args).await;
        let arguments = if tool == "digilock_set" {
            json!({"target": "dui", "command": command, "value": "0"})
        } else {
            json!({"target": "dui", "command": format!("{command}=0")})
        };
        let result = mcp.call(tool, arguments).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
        mcp.stop().await;
        timeout(DEADLINE, fake).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn config_defaults_and_environment_path_do_not_enable_global_writes() {
    for mode in ["allowlist", "denylist"] {
        let (dms, dms_port) = listener().await;
        let (dui, port) = listener().await;
        let config = ConfigFile::new(json!({"write_policy": {"mode": mode}}));
        let mut command = binary(&[
            "--dms-port".into(),
            dms_port.to_string(),
            "--dui-port".into(),
            port.to_string(),
        ]);
        command.env("DIGILOCK_CONFIG", &config.0);
        let mut mcp = Mcp::spawn(command);
        mcp.initialize().await;
        let info = mcp.call("digilock_info", json!({})).await;
        assert_eq!(info["structuredContent"]["allow_writes"], false);
        assert_eq!(info["structuredContent"]["write_policy"]["mode"], mode);
        assert_eq!(info["structuredContent"]["initial_values_count"], 0);
        for (tool, arguments) in [
            (
                "digilock_set",
                json!({"target": "dui", "command": "pid1:proportional", "value": "0.25"}),
            ),
            (
                "digilock_raw",
                json!({"target": "dui", "command": "pid1:proportional=0.25"}),
            ),
        ] {
            let result = mcp.call(tool, arguments).await;
            assert_eq!(result["isError"], true, "{mode}, {tool}: {result}");
            assert!(
                result["structuredContent"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("--allow-writes"),
                "{result}"
            );
        }
        assert_no_connection(&dui).await;
        let fake = tokio::spawn(async move {
            scripted_rci(
                dui,
                &[
                    ("pid1:proportional?", "pid1:proportional=5\r\n> "),
                    ("pid1:proportional?", "pid1:proportional=5\r\n> "),
                ],
            )
            .await;
        });
        for (tool, command) in [
            ("digilock_query", "pid1:proportional"),
            ("digilock_raw", "pid1:proportional?"),
        ] {
            let result = mcp
                .call(tool, json!({"target": "dui", "command": command}))
                .await;
            assert_eq!(result["isError"], false, "{mode}: {result}");
        }
        mcp.stop().await;
        timeout(DEADLINE, fake).await.unwrap().unwrap();
        assert_no_connection(&dms).await;
    }
}

#[tokio::test]
async fn startup_values_are_sent_before_initialize_in_order_with_optional_ports() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist"},
        "initial_values": [
            {"target": "dms", "command": "selected module", "value": "2"},
            {"target": "dui", "command": "pid1:proportional", "value": "0,25"},
            {"target": "dui", "port": other_port, "command": "pid1:proportional", "value": "0.5"},
            {"target": "dui", "command": "pid1:proportional", "value": "1e0"}
        ]
    }));
    let (completed_tx, completed_rx) = oneshot::channel();
    let fake = tokio::spawn(async move {
        let mut dms_stream = accept_rci(&dms).await;
        expect_rci_command(&mut dms_stream, "selected module=2").await;
        // No later endpoint may connect while an earlier startup write awaits its reply.
        assert_no_connection(&dui).await;
        assert_no_connection(&other_dui).await;
        dms_stream.get_mut().write_all(b"> ").await.unwrap();
        let mut dui_stream = accept_rci(&dui).await;
        expect_rci_command(&mut dui_stream, "pid1:proportional=0,25").await;
        assert_no_connection(&other_dui).await;
        dui_stream.get_mut().write_all(b"> ").await.unwrap();
        let mut other_stream = accept_rci(&other_dui).await;
        expect_rci_command(&mut other_stream, "pid1:proportional=0.5").await;
        assert!(
            timeout(
                Duration::from_millis(100),
                dui_stream.read_line(&mut String::new())
            )
            .await
            .is_err(),
            "startup sent a later write before the earlier reply"
        );
        other_stream.get_mut().write_all(b"> ").await.unwrap();
        expect_rci_command(&mut dui_stream, "pid1:proportional=1e0").await;
        dui_stream.get_mut().write_all(b"> ").await.unwrap();
        completed_tx.send(()).unwrap();
        expect_rci_command(&mut dui_stream, "pid1:proportional?").await;
        dui_stream
            .get_mut()
            .write_all(b"pid1:proportional=1\r\n> ")
            .await
            .unwrap();
        // No startup replay on initialize and no rollback when the MCP client exits.
        expect_rci_closed(&mut dms_stream).await;
        expect_rci_closed(&mut dui_stream).await;
        expect_rci_closed(&mut other_stream).await;
        assert_no_connection(&dms).await;
        assert_no_connection(&dui).await;
        assert_no_connection(&other_dui).await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        dui_port.to_string(),
    ]);
    let mut mcp = Mcp::spawn(binary(&args));
    // Intentionally do not send any MCP input until every startup write is observed.
    timeout(DEADLINE, completed_rx).await.unwrap().unwrap();
    mcp.initialize().await;
    let query = mcp
        .call(
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(query["isError"], false, "{query}");
    assert_eq!(query["structuredContent"]["port"], dui_port);
    mcp.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
}

#[tokio::test]
async fn scan_range_startup_queries_fresh_state_after_prior_writes_and_on_override() {
    let (dui, port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist", "scan_range": {"min": -2, "max": 6}},
        "initial_values": [
            {"target": "dui", "command": "OFFSET: VALUE", "value": "2"},
            {"target": "dui", "command": "scan:amplitude", "value": "8"},
            {"target": "dui", "command": "offset:value", "value": "1"},
            {"target": "dui", "port": other_port, "command": "SCAN: AMPLITUDE", "value": "8"},
            {"target": "dui", "command": "scan:amplitude", "value": "1"}
        ]
    }));
    let (completed_tx, completed_rx) = oneshot::channel();
    let fake = tokio::spawn(async move {
        let mut stream = accept_rci(&dui).await;
        for (command, reply) in [
            ("scan:amplitude?", "scan:amplitude=8\r\n> "),
            ("OFFSET: VALUE=2", "> "),
            ("offset:value?", "offset:value=2\r\n> "),
            ("scan:amplitude=8", "> "),
            // An external change after the preceding write makes this offset safe.
            ("scan:amplitude?", "scan:amplitude=1\r\n> "),
            ("offset:value=1", "> "),
        ] {
            expect_rci_command(&mut stream, command).await;
            if command == "offset:value=1" {
                assert_no_connection(&other_dui).await;
            }
            stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        }
        let mut other_stream = accept_rci(&other_dui).await;
        expect_rci_command(&mut other_stream, "offset:value?").await;
        other_stream
            .get_mut()
            .write_all(b"offset:value=2\r\n> ")
            .await
            .unwrap();
        expect_rci_command(&mut other_stream, "SCAN: AMPLITUDE=8").await;
        assert!(
            timeout(
                Duration::from_millis(100),
                stream.read_line(&mut String::new())
            )
            .await
            .is_err(),
            "later startup query preceded the override write reply"
        );
        other_stream.get_mut().write_all(b"> ").await.unwrap();
        expect_rci_command(&mut stream, "offset:value?").await;
        stream
            .get_mut()
            .write_all(b"offset:value=1\r\n> ")
            .await
            .unwrap();
        expect_rci_command(&mut stream, "scan:amplitude=1").await;
        stream.get_mut().write_all(b"> ").await.unwrap();
        completed_tx.send(()).unwrap();
        expect_rci_closed(&mut stream).await;
        expect_rci_closed(&mut other_stream).await;
        assert_no_connection(&dui).await;
        assert_no_connection(&other_dui).await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let mut mcp = Mcp::spawn(binary(&args));
    timeout(DEADLINE, completed_rx).await.unwrap().unwrap();
    mcp.initialize().await;
    mcp.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
}

#[tokio::test]
async fn scan_range_invalid_later_startup_value_preflights_before_any_connection() {
    let (dms, dms_port) = listener().await;
    let (dui, port) = listener().await;
    let (other_dui, other_port) = listener().await;
    for (command, value) in [
        ("SCAN: AMPLITUDE", "-0.01"),
        ("scan: amplitude", "NaN"),
        ("scan:amplitude", "inf"),
        ("scan:amplitude", "not-a-number"),
        ("OFFSET: VALUE", "NaN"),
        ("offset: value", "-inf"),
        ("offset:value", "1e999"),
        ("offset:value", "not-a-number"),
    ] {
        // No independent numeric limits: scan_range itself must parse proposed values.
        let config = ConfigFile::new(json!({
            "write_policy": {"mode": "denylist", "scan_range": {"max": 6}},
            "initial_values": [
                {"target": "dms", "command": "selected module", "value": "1"},
                {"target": "dui", "port": other_port, "command": command, "value": value}
            ]
        }));
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dms-port".into(),
            dms_port.to_string(),
            "--dui-port".into(),
            port.to_string(),
        ]);
        let error = assert_startup_failure(binary(&args)).await;
        assert!(
            error.contains("initial_values[1]"),
            "{command}={value}: {error}"
        );
        assert_no_connection(&dms).await;
        assert_no_connection(&dui).await;
        assert_no_connection(&other_dui).await;
    }
}

#[tokio::test]
async fn scan_range_unsafe_startup_aborts_later_commands_without_replay_or_rollback() {
    let (dui, port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist", "scan_range": {"min": -2, "max": 6}},
        "initial_values": [
            {"target": "dui", "command": "offset:value", "value": "2"},
            {"target": "dui", "command": "scan:amplitude", "value": "8"},
            {"target": "dui", "command": "offset:value", "value": "2"},
            {"target": "dui", "command": "pid1:proportional", "value": "0.25"},
            {"target": "dui", "port": other_port, "command": "scan:amplitude", "value": "1"}
        ]
    }));
    let fake = tokio::spawn(async move {
        scripted_rci(
            dui,
            &[
                ("scan:amplitude?", "scan:amplitude=8\r\n> "),
                ("offset:value=2", "> "),
                ("offset:value?", "offset:value=2\r\n> "),
                ("scan:amplitude=8", "> "),
                // Fresh state now violates both bounds; neither this nor later writes may run.
                ("scan:amplitude?", "scan:amplitude=10\r\n> "),
            ],
        )
        .await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let error = assert_startup_failure(binary(&args)).await;
    assert!(error.contains("initial_values[2]"), "{error}");
    assert!(
        error.contains("earlier writes may already have taken effect"),
        "{error}"
    );
    assert!(error.contains("no commands were replayed"), "{error}");
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    assert_no_connection(&other_dui).await;
}

#[tokio::test]
async fn invalid_config_is_rejected_before_any_tcp_connection() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    for contents in [
        "{",
        r#"{}"#,
        r#"{"write_policy":{"mode":"invalid"}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"min":2,"max":1}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"min":"NaN"}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"max":"inf"}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"min":1e999}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"max":-1e999}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"min":true}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"max":[]}}}"#,
        r#"{"write_policy":{"mode":"denylist","scan_range":{"min":null,"max":null}}}"#,
        r#"{"write_policy":{"mode":"denylist","limits":[{"target":"dui","command":"pid1:proportional","min":2,"max":1}]}}"#,
        r#"{"write_policy":{"mode":"denylist"},"initial_values":[{"target":"dui","command":"pid1:proportional","value":1}]}"#,
    ] {
        let config = ConfigFile::raw(contents);
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dms-port".into(),
            dms_port.to_string(),
            "--dui-port".into(),
            dui_port.to_string(),
        ]);
        let error = assert_startup_failure(binary(&args)).await;
        assert!(
            !error.is_empty(),
            "no configuration diagnostic for {contents}"
        );
        assert_no_connection(&dms).await;
        assert_no_connection(&dui).await;
    }
    let config = ConfigFile::new(json!({"write_policy": {"mode": "denylist"}}));
    let args = config.args();
    drop(config);
    let error = assert_startup_failure(binary(&args)).await;
    assert!(
        error.contains(&args[1]),
        "missing configuration path in error: {error}"
    );
    assert_no_connection(&dms).await;
    assert_no_connection(&dui).await;
}

#[tokio::test]
async fn invalid_later_startup_value_prevents_even_the_first_tcp_connection() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    for invalid in [
        json!({"target": "dui", "command": "pid2:proportional", "value": "0.5"}),
        json!({"target": "dui", "command": "pid1:proportional", "value": "2"}),
        json!({"target": "dui", "command": "pid1:lock:enable", "value": "true"}),
        json!({"target": "dui", "command": "pid1:lock:enable", "value": "0"}),
        json!({"target": "dui", "command": "scope:graph", "value": "1"}),
        json!({"target": "dui", "port": 0, "command": "pid1:proportional", "value": "0.5"}),
        json!({"target": "dui", "port": dms_port, "command": "pid1:proportional", "value": "0.5"}),
        json!({"target": "dms", "port": dui_port, "command": "selected module", "value": "2"}),
        json!({"target": "dui", "command": "pid1:proportional?", "value": "0.5"}),
        json!({"target": "dui", "command": "pid1:proportional", "value": "0.5\r\nprogram:exit=true"}),
    ] {
        let config = ConfigFile::new(json!({
            "write_policy": {
                "mode": "denylist",
                "commands": [{"target": "dui", "command": "pid2:proportional"}],
                "limits": [
                    {"target": "dui", "command": "pid1:proportional", "min": 0, "max": 1},
                    {"target": "dui", "command": "pid1:lock:enable", "allowed_values": ["false"]}
                ]
            },
            "initial_values": [
                {"target": "dms", "command": "selected module", "value": "1"},
                invalid
            ]
        }));
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dms-port".into(),
            dms_port.to_string(),
            "--dui-port".into(),
            dui_port.to_string(),
        ]);
        let error = assert_startup_failure(binary(&args)).await;
        assert!(error.contains("initial_values[1]"), "{invalid}: {error}");
        assert_no_connection(&dms).await;
        assert_no_connection(&dui).await;
    }
}

#[tokio::test]
async fn startup_requires_global_write_opt_in_before_connecting() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "allowlist", "commands": [
            {"target": "dui", "command": "pid1:proportional"}
        ]},
        "initial_values": [{"target": "dui", "command": "pid1:proportional", "value": "0.5"}]
    }));
    let mut args = config.args().to_vec();
    args.extend([
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        dui_port.to_string(),
    ]);
    let error = assert_startup_failure(binary(&args)).await;
    assert!(error.contains("initial_values[0]"), "{error}");
    assert!(error.contains("--allow-writes"), "{error}");
    assert_no_connection(&dms).await;
    assert_no_connection(&dui).await;
}

#[tokio::test]
async fn startup_device_error_aborts_without_later_writes_replay_or_rollback() {
    let (dms, dms_port) = listener().await;
    let (dui, port) = listener().await;
    let (other_dui, other_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist"},
        "initial_values": [
            {"target": "dui", "command": "pid1:proportional", "value": "0.25"},
            {"target": "dui", "command": "pid1:proportional", "value": "0.5"},
            {"target": "dui", "command": "pid1:proportional", "value": "0.75"},
            {"target": "dui", "port": other_port, "command": "pid1:proportional", "value": "1"}
        ]
    }));
    let fake = tokio::spawn(async move {
        scripted_rci(
            dui,
            &[
                ("pid1:proportional=0.25", "> "),
                (
                    "pid1:proportional=0.5",
                    "%% Error: rejected startup value\r\n> ",
                ),
            ],
        )
        .await;
    });
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let error = assert_startup_failure(binary(&args)).await;
    assert!(error.contains("initial_values[1]"), "{error}");
    assert!(
        error.contains("%% Error: rejected startup value"),
        "{error}"
    );
    assert!(
        error.contains("earlier writes may already have taken effect"),
        "{error}"
    );
    assert!(error.contains("no commands were replayed"), "{error}");
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    assert_no_connection(&other_dui).await;
    assert_no_connection(&dms).await;
}

#[cfg(unix)]
#[tokio::test]
async fn sigint_exits_even_when_parent_keeps_stdin_open() {
    let mut mcp = Mcp::start(&[]).await;
    assert!(
        Command::new("kill")
            .args(["-INT", &mcp.child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let status = timeout(DEADLINE, mcp.child.wait()).await.unwrap().unwrap();
    assert!(status.success(), "{status}");
}
