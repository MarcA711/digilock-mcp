//! End-to-end Streamable HTTP tests using only loopback HTTP and mock RCI servers.
//! No hardware, Python, HTTP client dependency, or stdin MCP traffic is involved.
// Shutdown checks use Unix SIGINT via `kill`, including when stdin is held open.
#![cfg(unix)]

use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, Command},
    sync::Notify,
    task::JoinHandle,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);
const PROTOCOL: &str = "2025-03-26";
const READY: &str = "MCP network service listening on http://";
const MAX_HTTP_BYTES: usize = 1_048_576;

#[derive(Clone, Default)]
struct Logs {
    lines: Arc<Mutex<Vec<String>>>,
    changed: Arc<Notify>,
}

impl Logs {
    fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    async fn wait_for(&self, predicate: impl Fn(&[String]) -> bool) {
        timeout(DEADLINE, async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if predicate(&self.snapshot()) {
                    return;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for stderr; captured: {:?}",
                self.snapshot()
            )
        });
    }
}

struct NetworkServer {
    child: Child,
    // Deliberately never write to or close stdin before waiting for SIGINT exit.
    _stdin: ChildStdin,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<()>,
    logs: Logs,
}

impl NetworkServer {
    fn spawn(extra: &[String]) -> Self {
        Self::spawn_on("127.0.0.1:0", extra)
    }

    fn spawn_on(listen: &str, extra: &[String]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_digilock-mcp"));
        command
            .args([
                "--host",
                "127.0.0.1",
                "--transport",
                "network",
                "--listen",
                listen,
            ])
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in [
            "DIGILOCK_HOST",
            "DIGILOCK_CONFIG",
            "DIGILOCK_ALLOW_WRITES",
            "DIGILOCK_DMS_PORT",
            "DIGILOCK_DUI_PORT",
            "DIGILOCK_TIMEOUT_MS",
            "DIGILOCK_MAX_RESPONSE_BYTES",
            "DIGILOCK_TRANSPORT",
            "DIGILOCK_LISTEN",
        ] {
            command.env_remove(name);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let logs = Logs::default();
        let capture = logs.clone();
        Self {
            child,
            _stdin: stdin,
            stdout: tokio::spawn(async move {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.unwrap();
                bytes
            }),
            stderr: tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Some(line) = lines.next_line().await.unwrap() {
                    capture.lines.lock().unwrap().push(line);
                    capture.changed.notify_waiters();
                }
            }),
            logs,
        }
    }

    async fn ready(&self) -> SocketAddr {
        self.logs
            .wait_for(|lines| lines.iter().any(|line| line.starts_with(READY)))
            .await;
        let lines = self.logs.snapshot();
        let ready: Vec<_> = lines
            .iter()
            .filter(|line| line.starts_with(READY))
            .collect();
        assert_eq!(ready.len(), 1, "readiness must be emitted once: {lines:?}");
        let address: SocketAddr = ready[0]
            .strip_prefix(READY)
            .unwrap()
            .strip_suffix("/mcp")
            .expect("readiness URL must end in /mcp")
            .parse()
            .unwrap();
        assert!(address.ip().is_loopback(), "{address}");
        assert_ne!(address.port(), 0, "readiness must report the bound port");
        address
    }

    async fn stop(self) -> Vec<String> {
        let status = timeout(
            DEADLINE,
            Command::new("kill")
                .args(["-INT", &self.child.id().unwrap().to_string()])
                .status(),
        )
        .await
        .expect("sending SIGINT timed out")
        .unwrap();
        assert!(status.success(), "sending SIGINT failed: {status}");
        self.wait_for_exit(true).await
    }

    async fn wait_for_exit(mut self, success: bool) -> Vec<String> {
        let status = timeout(DEADLINE, self.child.wait())
            .await
            .expect("network server did not exit with stdin held open")
            .unwrap();
        assert_eq!(
            status.success(),
            success,
            "unexpected network server exit status: {status}; stderr: {:?}",
            self.logs.snapshot()
        );
        let stdout = timeout(DEADLINE, self.stdout).await.unwrap().unwrap();
        assert!(
            stdout.is_empty(),
            "network stdout: {:?}",
            String::from_utf8_lossy(&stdout)
        );
        timeout(DEADLINE, self.stderr).await.unwrap().unwrap();
        self.logs.snapshot()
    }
}

struct ConfigFile(PathBuf);

impl ConfigFile {
    fn new(config: Value) -> Self {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "digilock-mcp-network-{}-{}.json",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let config_file = Self(path);
                    file.write_all(serde_json::to_string(&config).unwrap().as_bytes())
                        .unwrap();
                    return config_file;
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

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn rpc(&self, id: u64) -> Option<Value> {
        let content_type = self.header("content-type").unwrap_or("");
        if content_type.starts_with("application/json") {
            let message: Value = serde_json::from_slice(&self.body).ok()?;
            return (message["id"] == id).then_some(message);
        }
        if content_type.starts_with("text/event-stream") {
            let body = String::from_utf8_lossy(&self.body).replace("\r\n", "\n");
            // Only completed SSE events count; comments, IDs and notifications are ignored.
            for event in body.split("\n\n").take(body.matches("\n\n").count()) {
                let data = event
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|data| data.strip_prefix(' ').unwrap_or(data))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(message) = serde_json::from_str::<Value>(&data)
                    && message["id"] == id
                {
                    return Some(message);
                }
            }
        }
        None
    }
}

async fn http(
    address: SocketAddr,
    method: &str,
    session: Option<&str>,
    extra_headers: &[(&str, &str)],
    body: &Value,
    expected_id: Option<u64>,
) -> HttpResponse {
    timeout(DEADLINE, async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let body = serde_json::to_vec(body).unwrap();
        let host = extra_headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| (*value).to_owned()).unwrap_or_else(|| address.to_string());
        let mut request = format!(
            "{method} /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {PROTOCOL}\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(session) = session {
            request.push_str(&format!("Mcp-Session-Id: {session}\r\n"));
        }
        for (name, value) in extra_headers {
            if !name.eq_ignore_ascii_case("host") {
                request.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0, "HTTP EOF before status");
        assert!(line.starts_with("HTTP/1."), "invalid HTTP status: {line:?}");
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut response = HttpResponse { status, headers: Vec::new(), body: Vec::new() };
        let mut header_bytes = line.len();
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).await.unwrap() > 0, "HTTP EOF in headers");
            header_bytes += line.len();
            assert!(header_bytes <= MAX_HTTP_BYTES, "oversized HTTP headers");
            if line == "\r\n" { break; }
            let (name, value) = line.trim_end().split_once(':').expect("invalid HTTP header");
            response.headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
        if response.status == 204 || response.status == 304 { return response; }
        let chunked = response.header("transfer-encoding").is_some_and(|value| {
            value.split(',').any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
        });
        let length = response.header("content-length").map(|value| value.parse::<usize>().unwrap());
        if let Some(length) = length {
            assert!(length <= MAX_HTTP_BYTES, "oversized HTTP body");
        }
        loop {
            if expected_id.is_some_and(|id| response.rpc(id).is_some()) {
                // SSE POST streams need not close after their JSON-RPC response.
                break;
            }
            if chunked {
                line.clear();
                assert!(reader.read_line(&mut line).await.unwrap() > 0, "HTTP EOF before chunk");
                let size = usize::from_str_radix(line.trim().split(';').next().unwrap(), 16).unwrap();
                if size == 0 { break; }
                assert!(response.body.len() + size <= MAX_HTTP_BYTES, "oversized HTTP body");
                let start = response.body.len();
                response.body.resize(start + size, 0);
                reader.read_exact(&mut response.body[start..]).await.unwrap();
                let mut ending = [0; 2];
                reader.read_exact(&mut ending).await.unwrap();
                assert_eq!(&ending, b"\r\n", "invalid chunk terminator");
            } else {
                let remaining = length.map(|length| length - response.body.len());
                if remaining == Some(0) { break; }
                let mut bytes = [0; 4096];
                let limit = remaining.unwrap_or(bytes.len()).min(bytes.len());
                let count = reader.read(&mut bytes[..limit]).await.unwrap();
                if count == 0 {
                    assert!(length.is_none(), "HTTP EOF before Content-Length bytes");
                    break;
                }
                response.body.extend_from_slice(&bytes[..count]);
                assert!(response.body.len() <= MAX_HTTP_BYTES, "oversized HTTP body");
            }
        }
        response
    }).await.expect("HTTP request timed out")
}

struct Mcp {
    address: SocketAddr,
    session: String,
    next_id: u64,
}

fn initialize_message() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL, "capabilities": {},
            "clientInfo": {"name": "network-integration-test", "version": "1"}
        }
    })
}

impl Mcp {
    async fn connect(address: SocketAddr) -> Self {
        let response = http(address, "POST", None, &[], &initialize_message(), Some(1)).await;
        assert_eq!(response.status, 200, "initialize HTTP status");
        let init = response
            .rpc(1)
            .expect("missing initialize JSON-RPC response");
        assert_eq!(init["jsonrpc"], "2.0", "{init}");
        assert!(init.get("error").is_none(), "{init}");
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL, "{init}");
        assert!(
            init["result"]["capabilities"].get("tools").is_some(),
            "{init}"
        );
        let session = response
            .header("mcp-session-id")
            .expect("stateful transport must issue a session ID")
            .to_owned();
        assert!(!session.is_empty());
        let initialized = http(
            address,
            "POST",
            Some(&session),
            &[],
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
        )
        .await;
        assert_eq!(initialized.status, 202, "initialized notification status");
        Self {
            address,
            session,
            next_id: 2,
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let response = http(
            self.address,
            "POST",
            Some(&self.session),
            &[],
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            Some(id),
        )
        .await;
        assert_eq!(response.status, 200, "{method} HTTP status");
        let reply = response.rpc(id).unwrap_or_else(|| {
            panic!(
                "missing {method} reply: {:?}",
                String::from_utf8_lossy(&response.body)
            )
        });
        assert_eq!(reply["jsonrpc"], "2.0", "{reply}");
        assert_eq!(reply["id"], id, "{reply}");
        assert!(reply.get("error").is_none(), "{reply}");
        reply["result"].clone()
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }
}

async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn assert_no_connection(listener: &TcpListener) {
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "denied or local-only operation opened an RCI connection"
    );
}

fn rci_log(port: u16, command: &str) -> String {
    format!("RCI 127.0.0.1:{port} >> {command}")
}

async fn mock_rci(listener: TcpListener, logs: Logs, exchanges: &[(&str, &str)]) {
    let port = listener.local_addr().unwrap().port();
    let (mut stream, _) = timeout(DEADLINE, listener.accept()).await.unwrap().unwrap();
    stream.write_all(b"Mock DigiLock RCI\r\n> ").await.unwrap();
    let mut stream = BufReader::new(stream);
    for (index, (expected, reply)) in exchanges.iter().enumerate() {
        let mut command = String::new();
        let count = timeout(DEADLINE, async {
            tokio::select! {
                read = stream.read_line(&mut command) => read.unwrap(),
                connection = listener.accept() => panic!("MCP sessions opened a second RCI connection: {connection:?}"),
            }
        }).await.unwrap();
        assert!(count > 0, "RCI closed before {expected}");
        assert_eq!(command, format!("{expected}\r\n"));
        let line = rci_log(port, expected);
        let occurrences = exchanges[..=index]
            .iter()
            .filter(|(command, _)| command == expected)
            .count();
        // Hold the reply back: logging must happen on send, not after completion.
        logs.wait_for(|lines| lines.iter().filter(|entry| **entry == line).count() >= occurrences)
            .await;
        stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
    }
    let mut command = String::new();
    let count = timeout(DEADLINE, async {
        tokio::select! {
            read = stream.read_line(&mut command) => read.unwrap(),
            connection = listener.accept() => panic!("unexpected RCI reconnection: {connection:?}"),
        }
    })
    .await
    .unwrap();
    assert_eq!(count, 0, "unexpected additional RCI command: {command:?}");
    assert_no_connection(&listener).await;
}

fn assert_rci_logs(lines: &[String], port: u16, commands: &[&str]) {
    let actual: Vec<_> = lines
        .iter()
        .filter(|line| line.starts_with("RCI "))
        .cloned()
        .collect();
    let expected: Vec<_> = commands
        .iter()
        .map(|command| rci_log(port, command))
        .collect();
    assert_eq!(
        actual, expected,
        "each send must be logged exactly once and in order"
    );
}

#[tokio::test]
async fn handshake_discovery_and_read_only_rejection_never_connect_to_rci() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let server = NetworkServer::spawn(&[
        "--dms-port".into(),
        dms_port.to_string(),
        "--dui-port".into(),
        dui_port.to_string(),
    ]);
    let mut mcp = Mcp::connect(server.ready().await).await;
    let tools = mcp.request("tools/list", json!({})).await;
    let tools = tools["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    for name in [
        "digilock_info",
        "digilock_commands",
        "digilock_query",
        "digilock_set",
        "digilock_range",
        "digilock_help",
        "digilock_raw",
        "digilock_disconnect",
    ] {
        assert!(
            tools.iter().any(|tool| tool["name"] == name),
            "missing tool {name}"
        );
    }
    let setter = tools
        .iter()
        .find(|tool| tool["name"] == "digilock_set")
        .unwrap();
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
            "digilock_set",
            json!({"target": "dms", "command": "selected module", "value": "2"}),
        ),
        (
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional?\r\nprogram:exit=true"}),
        ),
    ] {
        let result = mcp.call(tool, args).await;
        assert_eq!(result["isError"], true, "{result}");
    }
    assert_no_connection(&dms).await;
    assert_no_connection(&dui).await;
    let lines = server.stop().await;
    assert!(
        !lines.iter().any(|line| line.starts_with("RCI ")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn query_range_and_raw_log_each_send_and_share_rci_across_two_mcp_sessions() {
    let (listener, port) = listener().await;
    let server = NetworkServer::spawn(&["--dui-port".into(), port.to_string()]);
    let logs = server.logs.clone();
    let fake = tokio::spawn(async move {
        mock_rci(
            listener,
            logs,
            &[
                (
                    "pid1:proportional?",
                    "pid1:proportional?\r\npid1:proportional=1,25\r\n> ",
                ),
                (
                    "pid1:proportional.range?",
                    "pid1:proportional.range=0 ... 10000\r\n> ",
                ),
                ("scope:graph?", "scope:graph=1;2\r\n3;4\r\n> "),
            ],
        )
        .await;
    });
    let address = server.ready().await;
    let mut first = Mcp::connect(address).await;
    let mut second = Mcp::connect(address).await;
    assert_ne!(
        first.session, second.session,
        "MCP sessions must be distinct"
    );
    let query = first
        .call(
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(query["isError"], false, "{query}");
    assert_eq!(query["structuredContent"]["value"], "1,25");
    assert_eq!(query["structuredContent"]["port"], port);
    let range = second
        .call(
            "digilock_range",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(range["isError"], false, "{range}");
    assert_eq!(range["structuredContent"]["value"], "0 ... 10000");
    let raw = first
        .call(
            "digilock_raw",
            json!({"target": "dui", "command": "scope:graph?"}),
        )
        .await;
    assert_eq!(raw["isError"], false, "{raw}");
    assert_eq!(raw["structuredContent"]["value"], "1;2\r\n3;4");
    // A different MCP session can close the same shared RCI connection.
    let disconnected = second
        .call("digilock_disconnect", json!({"target": "dui"}))
        .await;
    assert_eq!(disconnected["structuredContent"]["disconnected"], true);
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    let lines = server.stop().await;
    assert_rci_logs(
        &lines,
        port,
        &[
            "pid1:proportional?",
            "pid1:proportional.range?",
            "scope:graph?",
        ],
    );
}

#[tokio::test]
async fn startup_runs_once_before_readiness_and_logs_startup_and_live_safety_queries() {
    let (listener, port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist", "scan_range": {"min": -2, "max": 6}},
        "initial_values": [{"target": "dui", "command": "scan:amplitude", "value": "4"}]
    }));
    let mut args = config.args().to_vec();
    args.extend([
        "--allow-writes".into(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let server = NetworkServer::spawn(&args);
    let logs = server.logs.clone();
    let fake = tokio::spawn(async move {
        mock_rci(
            listener,
            logs,
            &[
                ("offset:value?", "offset:value=2\r\n> "),
                ("scan:amplitude=4", "> "),
                ("scan:amplitude?", "scan:amplitude=4\r\n> "),
                ("offset:value=1", "> "),
                ("offset:value?", "offset:value=1\r\n> "),
                ("scan:amplitude=2", "> "),
            ],
        )
        .await;
    });
    // Startup must finish without any stdin or HTTP initialize request.
    let address = server.ready().await;
    let before = server.logs.snapshot();
    let ready_index = before
        .iter()
        .position(|line| line.starts_with(READY))
        .unwrap();
    assert_rci_logs(
        &before[..ready_index],
        port,
        &["offset:value?", "scan:amplitude=4"],
    );
    let mut first = Mcp::connect(address).await;
    let mut second = Mcp::connect(address).await;
    assert_ne!(first.session, second.session);
    for mcp in [&mut first, &mut second] {
        let info = mcp.call("digilock_info", json!({})).await;
        assert_eq!(info["structuredContent"]["allow_writes"], true);
        assert_eq!(info["structuredContent"]["initial_values_count"], 1);
    }
    let set = first
        .call(
            "digilock_set",
            json!({"target": "dui", "command": "offset:value", "value": "1"}),
        )
        .await;
    assert_eq!(set["isError"], false, "{set}");
    let raw = second
        .call(
            "digilock_raw",
            json!({"target": "dui", "command": "scan:amplitude=2"}),
        )
        .await;
    assert_eq!(raw["isError"], false, "{raw}");
    let lines = server.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    assert_rci_logs(
        &lines,
        port,
        &[
            "offset:value?",
            "scan:amplitude=4",
            "scan:amplitude?",
            "offset:value=1",
            "offset:value?",
            "scan:amplitude=2",
        ],
    );
    assert_eq!(
        lines.iter().filter(|line| line.starts_with(READY)).count(),
        1
    );
}

#[tokio::test]
async fn graceful_sigint_with_stdin_unused_and_held_open_closes_active_rci() {
    let (listener, port) = listener().await;
    let server = NetworkServer::spawn(&["--dui-port".into(), port.to_string()]);
    let logs = server.logs.clone();
    let fake = tokio::spawn(async move {
        mock_rci(
            listener,
            logs,
            &[("pid1:proportional?", "pid1:proportional=1\r\n> ")],
        )
        .await;
    });
    let mut mcp = Mcp::connect(server.ready().await).await;
    let query = mcp
        .call(
            "digilock_query",
            json!({"target": "dui", "command": "pid1:proportional"}),
        )
        .await;
    assert_eq!(query["isError"], false, "{query}");
    let lines = server.stop().await;
    timeout(DEADLINE, fake).await.unwrap().unwrap();
    assert_rci_logs(&lines, port, &["pid1:proportional?"]);
}

#[tokio::test]
async fn occupied_listen_socket_prevents_startup_rci_writes() {
    let (occupied, _) = listener().await;
    let listen = occupied.local_addr().unwrap().to_string();
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist"},
        "initial_values": [
            {"target": "dms", "command": "selected module", "value": "2"},
            {"target": "dui", "command": "pid1:proportional", "value": "1"}
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
    let server = NetworkServer::spawn_on(&listen, &args);
    let lines = tokio::select! {
        lines = server.wait_for_exit(false) => lines,
        connection = dms.accept() => panic!("bind failure opened startup DMS connection: {connection:?}"),
        connection = dui.accept() => panic!("bind failure opened startup DUI connection: {connection:?}"),
    };
    let stderr = lines.join("\n");
    assert!(
        stderr.contains("MCP network listener") && stderr.to_ascii_lowercase().contains("bind"),
        "expected listen bind failure, got: {stderr}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with(READY)),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("RCI ")),
        "{lines:?}"
    );
    assert_no_connection(&dms).await;
    assert_no_connection(&dui).await;
    // Keep the occupied listener alive until the child has exited.
    drop(occupied);
}

#[tokio::test]
async fn allowed_custom_host_accepts_native_mcp_requests_but_not_hostile_headers() {
    let (listener, port) = listener().await;
    let server = NetworkServer::spawn(&[
        "--dui-port".into(),
        port.to_string(),
        "--allowed-host".into(),
        "digilock.test".into(),
        "--allowed-host".into(),
        "restricted.test:9000".into(),
    ]);
    let address = server.ready().await;
    let allowed = [("Host", "digilock.test")];
    // Connect to loopback directly; the custom Host needs no DNS entry or Origin.
    let initialized = http(
        address,
        "POST",
        None,
        &allowed,
        &initialize_message(),
        Some(1),
    )
    .await;
    assert_eq!(initialized.status, 200);
    let init = initialized
        .rpc(1)
        .expect("missing custom-host initialize reply");
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL, "{init}");
    let session = initialized
        .header("mcp-session-id")
        .expect("missing custom-host session");
    assert!(!session.is_empty());
    let notification = http(
        address,
        "POST",
        Some(session),
        &allowed,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        None,
    )
    .await;
    assert_eq!(notification.status, 202);
    let port_restricted = http(
        address,
        "POST",
        None,
        &[("Host", "restricted.test:9000")],
        &initialize_message(),
        Some(1),
    )
    .await;
    assert_eq!(port_restricted.status, 200);
    assert!(port_restricted.header("mcp-session-id").is_some());
    for headers in [
        vec![("Host", "restricted.test")],
        vec![("Host", "restricted.test:9001")],
        vec![("Host", "restricted.test:99999")],
        vec![("Host", "digilock.test.evil.example")],
        vec![
            ("Host", "digilock.test"),
            ("Origin", "https://evil.example"),
        ],
    ] {
        let rejected = http(address, "POST", None, &headers, &initialize_message(), None).await;
        assert!(
            matches!(rejected.status, 400 | 403 | 421),
            "hostile headers {headers:?}: HTTP {}",
            rejected.status
        );
        assert!(rejected.header("mcp-session-id").is_none());
    }
    let tools = http(
        address,
        "POST",
        Some(session),
        &allowed,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        Some(2),
    )
    .await;
    assert_eq!(tools.status, 200);
    let tools = tools.rpc(2).expect("missing custom-host discovery reply");
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 8);
    assert_no_connection(&listener).await;
    let lines = server.stop().await;
    assert!(
        !lines.iter().any(|line| line.starts_with("RCI ")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn invalid_allowed_hosts_prevent_startup_writes() {
    let (dms, dms_port) = listener().await;
    let (dui, dui_port) = listener().await;
    let config = ConfigFile::new(json!({
        "write_policy": {"mode": "denylist"},
        "initial_values": [
            {"target": "dms", "command": "selected module", "value": "2"},
            {"target": "dui", "command": "pid1:proportional", "value": "1"}
        ]
    }));
    for host in [
        "mcp.test:99999",
        "mcp.test:abc",
        "user@mcp.test",
        "",
        "https://mcp.test",
    ] {
        let mut args = config.args().to_vec();
        args.extend([
            "--allow-writes".into(),
            "--dms-port".into(),
            dms_port.to_string(),
            "--dui-port".into(),
            dui_port.to_string(),
            "--allowed-host".into(),
            host.into(),
        ]);
        let server = NetworkServer::spawn(&args);
        let lines = tokio::select! {
            lines = server.wait_for_exit(false) => lines,
            connection = dms.accept() => panic!("invalid allowlist opened DMS connection: {connection:?}"),
            connection = dui.accept() => panic!("invalid allowlist opened DUI connection: {connection:?}"),
        };
        assert!(
            lines.join("\n").contains("invalid --allowed-host"),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.starts_with(READY) || line.starts_with("RCI ")),
            "{lines:?}"
        );
    }
    assert_no_connection(&dms).await;
    assert_no_connection(&dui).await;
}

#[tokio::test]
async fn hostile_headers_cannot_write_or_delete_an_existing_session() {
    let (listener, port) = listener().await;
    let server = NetworkServer::spawn(&[
        "--allow-writes".into(),
        "--dui-port".into(),
        port.to_string(),
    ]);
    let address = server.ready().await;
    let mut mcp = Mcp::connect(address).await;
    let write = json!({
        "jsonrpc": "2.0", "id": 99, "method": "tools/call",
        "params": {"name": "digilock_set", "arguments": {
            "target": "dui", "command": "pid1:proportional", "value": "1"
        }}
    });
    for method in ["POST", "GET", "DELETE"] {
        for headers in [
            vec![("Host", "evil.example")],
            vec![("Origin", "https://evil.example")],
            vec![("Origin", "http://localhost")],
            vec![("Origin", "null")],
            vec![("Origin", "")],
            vec![
                ("Origin", "http://127.0.0.1"),
                ("Origin", "https://evil.example"),
            ],
        ] {
            let response = http(address, method, Some(&mcp.session), &headers, &write, None).await;
            assert!(
                matches!(response.status, 400 | 403 | 421),
                "{method} {headers:?}: HTTP {}",
                response.status
            );
            assert!(response.header("mcp-session-id").is_none());
        }
    }
    let tools = mcp.request("tools/list", json!({})).await;
    assert_eq!(tools["tools"].as_array().unwrap().len(), 8);
    assert_no_connection(&listener).await;
    let lines = server.stop().await;
    assert!(
        !lines.iter().any(|line| line.starts_with("RCI ")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn hostile_host_and_origin_are_rejected_without_affecting_legitimate_sessions() {
    let (listener, port) = listener().await;
    let server = NetworkServer::spawn(&["--dui-port".into(), port.to_string()]);
    let address = server.ready().await;
    for headers in [
        vec![("Host", "evil.example")],
        vec![("Host", "127.0.0.1.evil.example")],
        vec![("Origin", "https://evil.example")],
        vec![("Origin", "http://localhost.evil.example")],
    ] {
        let response = http(address, "POST", None, &headers, &initialize_message(), None).await;
        assert!(
            matches!(response.status, 400 | 403 | 421),
            "hostile headers {headers:?}: HTTP {}",
            response.status
        );
        assert!(
            response.header("mcp-session-id").is_none(),
            "rejected request created an MCP session"
        );
    }
    let mut mcp = Mcp::connect(address).await;
    let tools = mcp.request("tools/list", json!({})).await;
    assert_eq!(tools["tools"].as_array().unwrap().len(), 8);
    assert_no_connection(&listener).await;
    let lines = server.stop().await;
    assert!(
        !lines.iter().any(|line| line.starts_with("RCI ")),
        "{lines:?}"
    );
}
