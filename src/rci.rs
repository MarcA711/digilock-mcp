//! Persistent, prompt-framed DigiLock RCI connections over plain TCP.
//!
//! The protocol has no escaping or length field: a `> ` at the beginning of a
//! data line is indistinguishable from the prompt. Such payloads cannot be framed
//! reliably. Embedded `> ` elsewhere is ordinary data. Unexpected bytes received
//! after a prompt are rejected, rather than saved for the next transaction.
//!
//! A failed or cancelled transaction is never replayed. In particular, a timeout
//! does not imply that a set command had no effect on the device.

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Mutex,
    time::timeout,
};

/// One client owns at most one TCP connection per port on its configured host.
/// Share the client (for example, via `Arc`), rather than constructing multiple
/// clients for the same endpoint: the device permits only one connection.
pub struct RciClient {
    host: String,
    timeout: Duration,
    max_response_bytes: usize,
    sessions: Mutex<HashMap<u16, Arc<Mutex<Option<TcpStream>>>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RciResponse {
    pub command: String,
    /// Reply without the prompt, an exact first-line command echo, and one final
    /// line ending. All other whitespace and internal line endings are preserved.
    pub response: String,
    /// For a query `name?`, the text after an exact `name=` reply prefix.
    /// Includes multiline values; `None` for sets or replies without that prefix.
    pub value: Option<String>,
}

impl RciClient {
    /// `timeout` covers the entire transaction, including lock contention, DNS,
    /// connect, welcome, write, and reply. `max_response_bytes` bounds each wire
    /// frame (welcome or reply), including echo, line endings, and the prompt.
    pub fn new(host: String, timeout: Duration, max_response_bytes: usize) -> Self {
        Self {
            host,
            timeout,
            max_response_bytes,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn execute(&self, port: u16, command: &str) -> Result<RciResponse> {
        if port == 0 {
            bail!("RCI port must not be zero");
        }
        if command.trim().is_empty() {
            bail!("RCI command must not be empty or whitespace-only");
        }
        if command.chars().any(char::is_control) {
            bail!(
                "RCI command must not contain control characters (including CR, LF, NUL, or DEL)"
            );
        }

        timeout(self.timeout, self.execute_inner(port, command))
            .await
            .map_err(|_| {
                anyhow!(
                    "RCI transaction on {}:{} timed out after {:?}; it was not replayed",
                    self.host,
                    port,
                    self.timeout
                )
            })?
    }

    async fn execute_inner(&self, port: u16, command: &str) -> Result<RciResponse> {
        let slot = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .entry(port)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut guard = slot.lock().await;

        // Keep the stream OUT of the slot until a complete, valid reply arrives.
        // Errors and future cancellation then drop it automatically, even during
        // a partial write, without letting another request consume a stale reply.
        let mut stream = match guard.take() {
            Some(stream) => stream,
            None => {
                let mut stream = TcpStream::connect((self.host.as_str(), port))
                    .await
                    .with_context(|| format!("connecting to RCI {}:{}", self.host, port))?;
                read_frame(&mut stream, self.max_response_bytes)
                    .await
                    .context("reading RCI welcome")?;
                stream
            }
        };

        stream
            .write_all(command.as_bytes())
            .await
            .context("writing RCI command")?;
        stream
            .write_all(b"\r\n")
            .await
            .context("writing RCI command terminator")?;
        let frame = read_frame(&mut stream, self.max_response_bytes)
            .await
            .context("reading RCI reply")?;
        let response = strip_reply(&frame, command).to_owned();

        // A device error is still a correctly framed transaction. Preserve the
        // connection before returning it as an error to the caller.
        *guard = Some(stream);
        if response.lines().any(|line| line.starts_with("%% Error: ")) {
            bail!("RCI device error: {response}");
        }

        let value = command.strip_suffix('?').and_then(|name| {
            response
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::to_owned)
        });
        Ok(RciResponse {
            command: command.to_owned(),
            response,
            value,
        })
    }

    /// Waits for any transaction on this port, then closes its session. Returns
    /// whether an open session was removed. Other ports remain independent.
    pub async fn disconnect(&self, port: u16) -> bool {
        let slot = self.sessions.lock().await.get(&port).cloned();
        match slot {
            Some(slot) => slot.lock().await.take().is_some(),
            None => false,
        }
        // Slots intentionally remain in the map: removing them could create two
        // locks/connections for the same port while an execute is already queued.
    }
}

async fn read_frame(stream: &mut TcpStream, limit: usize) -> Result<String> {
    let mut bytes = Vec::new();
    let mut scan_from = 0;
    let mut chunk = [0_u8; 4096];
    loop {
        if bytes.len() == limit {
            bail!("RCI frame exceeded maximum of {limit} incoming bytes before its prompt");
        }
        let capacity = chunk.len().min(limit - bytes.len());
        let count = stream
            .read(&mut chunk[..capacity])
            .await
            .context("reading RCI TCP stream")?;
        if count == 0 {
            bail!("RCI connection closed (EOF) before a complete prompt");
        }
        bytes.extend_from_slice(&chunk[..count]);

        for index in scan_from..bytes.len().saturating_sub(1) {
            if bytes[index..index + 2] == *b"> " && (index == 0 || bytes[index - 1] == b'\n') {
                if index + 2 != bytes.len() {
                    bail!("RCI framing error: unexpected bytes after the prompt");
                }
                bytes.truncate(index);
                return String::from_utf8(bytes).context("RCI frame contains invalid UTF-8");
            }
        }
        // Revisit the last byte: it might be the first half of a split prompt.
        scan_from = bytes.len().saturating_sub(1);
    }
}

fn strip_reply<'a>(frame: &'a str, command: &str) -> &'a str {
    let without_echo = match frame.split_once('\n') {
        Some((first, rest)) if first.strip_suffix('\r').unwrap_or(first) == command => rest,
        _ => frame,
    };
    without_echo
        .strip_suffix("\r\n")
        .or_else(|| without_echo.strip_suffix('\n'))
        .unwrap_or(without_echo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        net::{TcpListener, tcp::OwnedReadHalf},
        sync::{Barrier, oneshot},
        time::sleep,
    };

    const TEST_TIMEOUT: Duration = Duration::from_secs(3);

    async fn listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    fn client(timeout: Duration, limit: usize) -> RciClient {
        RciClient::new("127.0.0.1".to_owned(), timeout, limit)
    }

    async fn command(reader: &mut BufReader<OwnedReadHalf>, expected: &str) {
        let mut line = String::new();
        timeout(TEST_TIMEOUT, reader.read_line(&mut line))
            .await
            .expect("server timed out waiting for command")
            .unwrap();
        assert_eq!(line, format!("{expected}\r\n"));
    }

    async fn closed(reader: &mut BufReader<OwnedReadHalf>) {
        let mut byte = [0];
        assert_eq!(
            timeout(TEST_TIMEOUT, reader.read(&mut byte))
                .await
                .expect("client did not close connection")
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn fragmented_welcome_echo_query_set_and_no_reply() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            write.write_all(b"Welcome\r\n>").await.unwrap();
            sleep(Duration::from_millis(10)).await;
            write.write_all(b" ").await.unwrap();
            command(&mut read, "gain?").await;
            write.write_all(b"gain?\r\ngain=12\r\n>").await.unwrap();
            sleep(Duration::from_millis(10)).await;
            write.write_all(b" ").await.unwrap();
            command(&mut read, "gain=20").await;
            write.write_all(b"gain=20\r\n> ").await.unwrap();
            command(&mut read, "enable=true").await;
            write.write_all(b"> ").await.unwrap();
            command(&mut read, "gain.range?").await;
            write
                .write_all(b"gain.range=0 ... 100\r\n> ")
                .await
                .unwrap();
            closed(&mut read).await;
        });
        let client = client(TEST_TIMEOUT, 1024);
        let reply = client.execute(port, "gain?").await.unwrap();
        assert_eq!(reply.command, "gain?");
        assert_eq!(reply.response, "gain=12");
        assert_eq!(reply.value.as_deref(), Some("12"));
        for set in ["gain=20", "enable=true"] {
            let reply = client.execute(port, set).await.unwrap();
            assert_eq!(reply.response, "");
            assert_eq!(reply.value, None);
        }
        assert_eq!(
            client
                .execute(port, "gain.range?")
                .await
                .unwrap()
                .value
                .as_deref(),
            Some("0 ... 100")
        );
        assert!(client.disconnect(port).await);
        assert!(!client.disconnect(port).await);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn multiline_arrays_embedded_prompt_and_exact_echo_only() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            write.write_all(b"> ").await.unwrap();
            command(&mut read, "names?").await;
            write
                .write_all(b"names?\r\nnames=[\r\n  \"a > b\",\r\n  \"c\"\r\n]\r\n> ")
                .await
                .unwrap();
            command(&mut read, "help?").await;
            write.write_all(b" help?\nhelp=x > y\n\n> ").await.unwrap();
            command(&mut read, "plain?").await;
            write
                .write_all(b"plain?extra\r\ntext\r\nplain?\r\n> ")
                .await
                .unwrap();
        });
        let client = client(TEST_TIMEOUT, 1024);
        let reply = client.execute(port, "names?").await.unwrap();
        assert_eq!(reply.response, "names=[\r\n  \"a > b\",\r\n  \"c\"\r\n]");
        assert_eq!(
            reply.value.as_deref(),
            Some("[\r\n  \"a > b\",\r\n  \"c\"\r\n]")
        );
        let reply = client.execute(port, "help?").await.unwrap();
        assert_eq!(reply.response, " help?\nhelp=x > y\n");
        assert_eq!(reply.value, None);
        assert_eq!(
            client.execute(port, "plain?").await.unwrap().response,
            "plain?extra\r\ntext\r\nplain?"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn device_errors_retain_the_same_connection() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            write.write_all(b"> ").await.unwrap();
            command(&mut read, "bad=1").await;
            write
                .write_all(b"bad=1\r\n%% Error: bad command-bad\r\n> ")
                .await
                .unwrap();
            command(&mut read, "bad?").await;
            write
                .write_all(b"status\r\n%% Error: bad parameter-bad\r\n> ")
                .await
                .unwrap();
            command(&mut read, "good?").await;
            write.write_all(b"good=1\r\n> ").await.unwrap();
        });
        let client = client(TEST_TIMEOUT, 1024);
        for cmd in ["bad=1", "bad?"] {
            let error = client.execute(port, cmd).await.unwrap_err().to_string();
            assert!(error.contains("%% Error: "), "{error}");
        }
        assert_eq!(
            client
                .execute(port, "good?")
                .await
                .unwrap()
                .value
                .as_deref(),
            Some("1")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn eof_invalidates_and_next_request_reconnects_without_replay() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"> ").await.unwrap();
            let (read, write) = stream.into_split();
            command(&mut BufReader::new(read), "set=1").await;
            drop(write);
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"New welcome\r\n> ").await.unwrap();
            let (read, mut write) = stream.into_split();
            command(&mut BufReader::new(read), "next?").await;
            write.write_all(b"next=2\r\n> ").await.unwrap();
        });
        let client = client(TEST_TIMEOUT, 1024);
        let error = format!("{:#}", client.execute(port, "set=1").await.unwrap_err());
        assert!(error.contains("EOF"), "{error}");
        assert!(!client.disconnect(port).await);
        assert_eq!(
            client
                .execute(port, "next?")
                .await
                .unwrap()
                .value
                .as_deref(),
            Some("2")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn welcome_eof_sends_no_command() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"Welcome\r\n>").await.unwrap();
            stream.shutdown().await.unwrap();
            let (read, _) = stream.into_split();
            closed(&mut BufReader::new(read)).await;
        });
        let client = client(TEST_TIMEOUT, 1024);
        let error = format!("{:#}", client.execute(port, "set=1").await.unwrap_err());
        assert!(
            error.contains("welcome") && error.contains("EOF"),
            "{error}"
        );
        assert!(!client.disconnect(port).await);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn size_cap_applies_to_welcome_and_reply() {
        for welcome in [true, false] {
            let (listener, port) = listener().await;
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                if !welcome {
                    write.write_all(b"> ").await.unwrap();
                    command(&mut read, "large?").await;
                }
                write.write_all(b"0123456789abcdef").await.unwrap();
                closed(&mut read).await;
            });
            let client = client(TEST_TIMEOUT, 16);
            let error = format!("{:#}", client.execute(port, "large?").await.unwrap_err());
            assert!(error.contains("maximum of 16"), "{error}");
            assert!(!client.disconnect(port).await);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn exact_size_limit_accepts_prompt() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"> ").await.unwrap();
            command(&mut BufReader::new(read), "x?").await;
            write.write_all(b"x=1\n> ").await.unwrap();
        });
        let client = client(TEST_TIMEOUT, 6);
        assert_eq!(
            client.execute(port, "x?").await.unwrap().value.as_deref(),
            Some("1")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_utf8_and_trailing_prompt_data_invalidate() {
        for (payload, expected) in [
            (&b"x=\xff\r\n> "[..], "invalid UTF-8"),
            (&b"x=1\r\n> trailing"[..], "framing error"),
        ] {
            let (listener, port) = listener().await;
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                write.write_all(b"> ").await.unwrap();
                command(&mut read, "x?").await;
                write.write_all(payload).await.unwrap();
                closed(&mut read).await;
            });
            let client = client(TEST_TIMEOUT, 1024);
            let error = format!("{:#}", client.execute(port, "x?").await.unwrap_err());
            assert!(error.contains(expected), "{error}");
            assert!(!client.disconnect(port).await);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_utf8_welcome_sends_no_command() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"\xff\n> ").await.unwrap();
            closed(&mut BufReader::new(read)).await;
        });
        let client = client(TEST_TIMEOUT, 1024);
        let error = format!("{:#}", client.execute(port, "set=1").await.unwrap_err());
        assert!(
            error.contains("welcome") && error.contains("invalid UTF-8"),
            "{error}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_invalidates_then_reconnects_without_replaying_set() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            write.write_all(b"> ").await.unwrap();
            command(&mut read, "set=1").await;
            write.write_all(b"stale partial\r\n>").await.unwrap();
            closed(&mut read).await;
            drop(write);
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"> ").await.unwrap();
            command(&mut BufReader::new(read), "next?").await;
            write.write_all(b"next=fresh\r\n> ").await.unwrap();
        });
        let client = client(Duration::from_millis(150), 1024);
        let error = client.execute(port, "set=1").await.unwrap_err().to_string();
        assert!(error.contains("timed out"), "{error}");
        assert!(!client.disconnect(port).await);
        assert_eq!(
            client
                .execute(port, "next?")
                .await
                .unwrap()
                .value
                .as_deref(),
            Some("fresh")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_covers_welcome_and_closes_connection() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"Welcome without prompt").await.unwrap();
            closed(&mut BufReader::new(read)).await;
        });
        let client = client(Duration::from_millis(100), 1024);
        assert!(
            client
                .execute(port, "set=1")
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(!client.disconnect(port).await);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn one_deadline_covers_welcome_and_reply_together() {
        let (listener, port) = listener().await;
        let (release_tx, release_rx) = oneshot::channel();
        let (received_tx, received_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            release_rx.await.unwrap();
            write.write_all(b"> ").await.unwrap();
            command(&mut read, "set=1").await;
            received_tx.send(()).unwrap();
            closed(&mut read).await;
        });
        let client = client(Duration::from_millis(200), 1024);
        let request = client.execute(port, "set=1");
        tokio::pin!(request);
        tokio::select! {
            result = &mut request => panic!("request finished early: {result:?}"),
            _ = sleep(Duration::from_millis(120)) => {},
        }
        release_tx.send(()).unwrap();
        tokio::select! {
            result = &mut request => panic!("request finished before write: {result:?}"),
            result = received_rx => result.unwrap(),
        }
        // A reset-per-phase timeout would still have over 100ms left here.
        assert!(
            timeout(Duration::from_millis(130), &mut request)
                .await
                .expect("transaction deadline was reset after welcome")
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn queued_timeout_does_not_invalidate_active_session() {
        let (listener, port) = listener().await;
        let client = client(Duration::from_millis(60), 1024);
        // Hold the same slot that an active execute would hold, without relying
        // on two wall-clock deadlines firing in a particular order.
        let slot = Arc::new(Mutex::new(None));
        client.sessions.lock().await.insert(port, slot.clone());
        let mut guard = slot.lock().await;
        let connected = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        *guard = Some(connected);
        assert!(
            client
                .execute(port, "queued=1")
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        let mut byte = [0];
        assert!(
            timeout(Duration::from_millis(20), peer.read(&mut byte))
                .await
                .is_err()
        );
        assert!(guard.is_some());
        drop(guard);
        assert!(client.disconnect(port).await);
        assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn concurrent_requests_on_same_port_are_serialized() {
        let (listener, port) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            write.write_all(b"> ").await.unwrap();
            for _ in 0..2 {
                let mut line = String::new();
                read.read_line(&mut line).await.unwrap();
                assert!(line == "a?\r\n" || line == "b?\r\n", "{line:?}");
                assert!(
                    timeout(Duration::from_millis(30), read.fill_buf())
                        .await
                        .is_err()
                );
                let name = line.trim_end().strip_suffix('?').unwrap();
                write
                    .write_all(format!("{name}=ok\r\n> ").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let client = client(TEST_TIMEOUT, 1024);
        let (a, b) = tokio::join!(client.execute(port, "a?"), client.execute(port, "b?"));
        assert_eq!(a.unwrap().response, "a=ok");
        assert_eq!(b.unwrap().response, "b=ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn separate_ports_progress_concurrently() {
        let barrier = Arc::new(Barrier::new(2));
        let (first, first_port) = listener().await;
        let (second, second_port) = listener().await;
        let mut servers = Vec::new();
        for listener in [first, second] {
            let barrier = barrier.clone();
            servers.push(tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                write.write_all(b"> ").await.unwrap();
                command(&mut BufReader::new(read), "x?").await;
                timeout(TEST_TIMEOUT, barrier.wait()).await.unwrap();
                write.write_all(b"x=ok\r\n> ").await.unwrap();
            }));
        }
        let client = client(TEST_TIMEOUT, 1024);
        let (first, second) = tokio::join!(
            client.execute(first_port, "x?"),
            client.execute(second_port, "x?")
        );
        assert_eq!(first.unwrap().value.as_deref(), Some("ok"));
        assert_eq!(second.unwrap().value.as_deref(), Some("ok"));
        for server in servers {
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn external_cancellation_invalidates_in_flight_session() {
        let (listener, port) = listener().await;
        let (received_tx, received_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            write.write_all(b"> ").await.unwrap();
            command(&mut read, "set=1").await;
            received_tx.send(()).unwrap();
            closed(&mut read).await;
        });
        let client = Arc::new(client(TEST_TIMEOUT, 1024));
        let request_client = client.clone();
        let request = tokio::spawn(async move { request_client.execute(port, "set=1").await });
        received_rx.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(!client.disconnect(port).await);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn command_injection_and_invalid_ports_are_rejected_before_connect() {
        let (listener, port) = listener().await;
        let client = client(TEST_TIMEOUT, 1024);
        for command in [
            "", "   ", "\n", "x\r", "x\nset=1", "x\0", "x\t", "x\u{1b}", "x\u{7f}", "x\u{85}",
        ] {
            assert!(client.execute(port, command).await.is_err(), "{command:?}");
        }
        for control in 0_u8..=31 {
            let command = format!("x{}y", char::from(control));
            assert!(client.execute(port, &command).await.is_err());
        }
        assert!(
            client
                .execute(0, "x?")
                .await
                .unwrap_err()
                .to_string()
                .contains("port")
        );
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        assert!(client.sessions.lock().await.is_empty());
        assert!(!client.disconnect(port).await);
        assert!(!client.disconnect(0).await);
    }

    #[test]
    fn stripping_preserves_data_whitespace_and_only_one_exact_echo() {
        assert_eq!(strip_reply("x?\r\nx?\r\n", "x?"), "x?");
        assert_eq!(strip_reply("x? \r\nx=1\r\n", "x?"), "x? \r\nx=1");
        assert_eq!(strip_reply("\r\nx= 1 \r\n\r\n", "x?"), "\r\nx= 1 \r\n");
        assert_eq!(strip_reply("x?\n", "x?"), "");
        assert_eq!(strip_reply("", "x?"), "");
    }
}
