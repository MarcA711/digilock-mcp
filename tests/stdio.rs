//! End-to-end tests of the shipped binary, using MCP JSON-RPC over stdio and
//! fake local RCI servers. No DigiLock hardware or Python dependencies required.
use std::{process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_digilock-mcp"));
        command
            .args(["--host", "127.0.0.1"])
            .args(extra)
            .env_remove("DIGILOCK_ALLOW_WRITES")
            .env_remove("DIGILOCK_DMS_PORT")
            .env_remove("DIGILOCK_DUI_PORT")
            .env_remove("DIGILOCK_TIMEOUT_MS")
            .env_remove("DIGILOCK_MAX_RESPONSE_BYTES")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut client = Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 1,
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26", "capabilities": {},
                    "clientInfo": {"name": "integration-test", "version": "1"}
                }),
            )
            .await;
        assert!(init.get("result").is_some(), "{init}");
        client
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        client
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
