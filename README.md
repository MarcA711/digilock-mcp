# DigiLock MCP wrapper (Rust)

A stdio [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server for the **TOPTICA DigiLock 110 Remote Control Interface (RCI)**. It exposes command discovery, queries, range/help requests, and explicitly opt-in writes to MCP clients.

The implementation follows the bundled [DigiLock 110 RCI manual](docs/DigiLock-RCI_Manual.pdf), **M-038 version 05, February 2023**. This is a TCP wrapper around the Windows DigiLock software, **not a direct USB driver** or a replacement for that software.

```text
MCP client <-- stdio --> digilock-mcp <-- TCP RCI --> Windows DMS / DUI <-- USB --> DigiLock 110
```

- **DMS** (DigiLock Module Server): discovers and manages modules and starts their interfaces.
- **DUI** (DigiLock User Interface): controls one module; each instance has its own RCI TCP port.

## Prerequisites

- Rust **1.88 or newer** and Cargo, as required by `Cargo.toml`.
- A Windows PC running DigiLock software **1.5.4.70 or newer**, with the DigiLock module connected to that PC by USB and the DMS running. The desired DUI must be running before querying its controls; start it in Windows or explicitly opt into DMS writes as described below.
- TCP reachability from the wrapper to the Windows host's RCI ports. The wrapper can run on a different machine; `--host` identifies the **Windows PC**, not the USB device.
- An MCP client that supports launching stdio servers.

The defaults are DMS port `60000` and DUI port `60001`, but discover the actual DUI ports rather than assuming them. Windows firewall rules should allow only the intended trusted clients.

## Build, run, and test

Run these commands from `/workspaces/digilock-mcp`:

```sh
cargo build --release --locked
./target/release/digilock-mcp --help
./target/release/digilock-mcp --host 192.168.1.10
```

Replace `192.168.1.10` with your Windows host's address. This launch is **read-only by default**. The process speaks MCP on stdin/stdout; it is not an interactive RCI command shell and will wait for an MCP client when launched directly. **Stdout belongs exclusively to MCP protocol traffic**; diagnostics go to stderr. Do not inject banners or redirect logs into stdout.

To permit writes, explicitly restart with either:

```sh
./target/release/digilock-mcp --host 192.168.1.10 --allow-writes
```

or:

```sh
DIGILOCK_ALLOW_WRITES=true ./target/release/digilock-mcp --host 192.168.1.10
```

On Windows, the built executable has an `.exe` suffix. Use the absolute path to the binary built for the machine that runs your MCP client.

Run the automated tests with:

```sh
cargo test --locked
```

Tests exercise CLI defaults, catalog validation, routing/write guards, and TCP framing, timeouts, cancellation, and connection reuse using **local loopback mock servers**. End-to-end tests also launch the binary over MCP stdio to check tool discovery, routing, write policy, queued/in-flight MCP cancellation, and SIGINT shutdown. They do not test Windows DigiLock software or physical hardware. Loopback TCP binding and subprocess execution must be permitted in the test environment.

## MCP client configuration

For clients using the standard `mcpServers` configuration shape:

```json
{
  "mcpServers": {
    "digilock": {
      "command": "/workspaces/digilock-mcp/target/release/digilock-mcp",
      "args": ["--host", "192.168.1.10"]
    }
  }
}
```

Build first, substitute your Windows host address, and adjust the absolute executable path if the project is elsewhere. The client launches the built binary directly, not `cargo run`. For intentional write access, append `"--allow-writes"` to `args` or add `"env": {"DIGILOCK_ALLOW_WRITES": "true"}` to the server entry. Leave both absent for the default read-only policy.

## CLI and environment configuration

Explicit CLI options take precedence over their corresponding environment variables.

| CLI option | Environment variable | Default | Meaning |
| --- | --- | --- | --- |
| `--host` | `DIGILOCK_HOST` | `127.0.0.1` | Windows host running DMS/DUI; localhost works only when that software is reachable there. |
| `--dms-port` | `DIGILOCK_DMS_PORT` | `60000` | Module Server RCI port. |
| `--dui-port` | `DIGILOCK_DUI_PORT` | `60001` | Default DUI RCI port; a tool request can select another discovered DUI port. |
| `--timeout-ms` | `DIGILOCK_TIMEOUT_MS` | `5000` | Whole RCI transaction deadline in milliseconds, including queue wait, DNS, connect, welcome, send, and reply. |
| `--max-response-bytes` | `DIGILOCK_MAX_RESPONSE_BYTES` | `8388608` (8 MiB) | Limit for each welcome or reply frame, including echo, line endings, and prompt; increase deliberately for large graphs. |
| `--allow-writes` | `DIGILOCK_ALLOW_WRITES` | `false` | Allows hardware writes and arbitrary raw commands; use environment value `true` to opt in. |

`--help` and `--version` print information and exit. Configuration requires a nonblank host, nonzero and distinct DMS/default DUI ports, a positive timeout, and a response limit of at least two bytes.

## Tools and exact argument fields

All endpoint/control fields are **flattened at the top level** of the arguments object. Do not nest them under `endpoint` or `control`.

| Tool | Argument fields | Operation |
| --- | --- | --- |
| `digilock_info` | None: `{}` | Returns configuration, write policy, manual version, and catalog entry count; no hardware connection. |
| `digilock_commands` | Optional `target`, `filter`, `writable` | Searches the bundled static catalog offline. |
| `digilock_query` | Required `target`, `command`; optional `port` | Sends `command?`. |
| `digilock_range` | Required `target`, `command`; optional `port` | Sends `command.range?`. |
| `digilock_help` | Required `target`, `command`; optional `port` | Sends `command.help?`. |
| `digilock_set` | Required `target`, `command`, `value`; optional `port` | Sends `command=value`; requires write opt-in. |
| `digilock_raw` | Required `target`, `command`; optional `port` | Sends one complete RCI command, subject to the write policy. |
| `digilock_disconnect` | Required `target`; optional `port` | Closes this wrapper's TCP session for the endpoint, not its hardware connection. |

Field types and semantics:

- `target`: the exact JSON string `"dms"` or `"dui"`. Required for endpoint tools; optional for catalog search.
- `port`: optional JSON integer in `1..65535`, or `null` to use the configured default for the target. For DMS, omit it or use the configured DMS port. For DUI, it cannot equal the configured DMS port.
- `command`: required JSON string. Query/set/range/help tools take the **exact control name**, preserving spaces and case, without `?`, `=`, `.range?`, or `.help?`. Names cannot have leading/trailing whitespace and may contain only ASCII letters, digits, spaces, `:`, `/`, `_`, and `-`. Raw requests instead take a complete RCI command, including its query/assignment syntax, with no control characters (including CR/LF).
- `value`: required **nonempty JSON string** for `digilock_set`, not a JSON number or boolean. Documented boolean controls require exactly `"true"` or `"false"`. Numeric strings must preserve the Windows host's decimal separator, such as `"0,25"` rather than `"0.25"` where applicable. Enum labels must match firmware values. No control characters are allowed.
- `filter`: optional JSON string or `null`; case-insensitive substring search of catalog names and descriptions.
- `writable`: optional JSON boolean or `null`; `true` selects catalog entries with set access, `false` selects entries without set access. This filters catalog metadata, **not** the current server write policy.

### Example tool calls

The following objects are the `params` bodies of MCP `tools/call` requests; your client supplies the JSON-RPC envelope.

Inspect configuration and search documented PID controls, without contacting Windows:

```json
{"name":"digilock_info","arguments":{}}
```

```json
{"name":"digilock_commands","arguments":{"target":"dui","filter":"pid1","writable":true}}
```

Discover the live commands on DMS:

```json
{"name":"digilock_query","arguments":{"target":"dms","command":"commandlist"}}
```

## Discover and connect to the correct module

1. Query all four DMS module lists:

   ```json
   {"name":"digilock_query","arguments":{"target":"dms","command":"modules:names"}}
   ```

   ```json
   {"name":"digilock_query","arguments":{"target":"dms","command":"modules:serial numbers"}}
   ```

   ```json
   {"name":"digilock_query","arguments":{"target":"dms","command":"modules:port numbers"}}
   ```

   ```json
   {"name":"digilock_query","arguments":{"target":"dms","command":"modules:connection status"}}
   ```

   Interpret the returned lists together to identify the intended module by name and serial number. They remain strings; the wrapper does not join or parse them.

2. Inspect the current selection and the firmware's selection range/help:

   ```json
   {"name":"digilock_query","arguments":{"target":"dms","command":"selected module"}}
   ```

   ```json
   {"name":"digilock_range","arguments":{"target":"dms","command":"selected module"}}
   ```

   ```json
   {"name":"digilock_help","arguments":{"target":"dms","command":"selected module"}}
   ```

   **Do not assume zero-based or one-based module numbering**, or derive a DUI port from that index. Confirm the intended index against the firmware/Windows DMS list.

3. If the module is already connected, proceed to its listed DUI port. Otherwise, have the operator select/connect it in Windows. Alternatively, with explicit write opt-in and operator approval, set `selected module` and then `module:connect` to `"true"`. For example, **only if the verified intended index is `2`**:

   ```json
   {"name":"digilock_set","arguments":{"target":"dms","command":"selected module","value":"2"}}
   ```

   ```json
   {"name":"digilock_set","arguments":{"target":"dms","command":"module:connect","value":"true"}}
   ```

   These are state-changing operations, not automatic discovery steps. Re-query connection status and port numbers after connecting.

4. Route subsequent requests to the **actual DUI port returned for that module**. For example, if the list reports port `60007`, use that exact port, not the default `60001`:

   ```json
   {"name":"digilock_query","arguments":{"target":"dui","port":60007,"command":"commandlist"}}
   ```

   ```json
   {"name":"digilock_query","arguments":{"target":"dui","port":60007,"command":"pid1:proportional"}}
   ```

   ```json
   {"name":"digilock_range","arguments":{"target":"dui","port":60007,"command":"pid1:proportional"}}
   ```

   ```json
   {"name":"digilock_help","arguments":{"target":"dui","port":60007,"command":"pid1:proportional"}}
   ```

   All DUI examples below use this hypothetical discovered port; replace it with your module's actual port. Query range/help before an approved set, use a string value, and query again afterward to verify the resulting state. A successful write can return empty text; that is not an independent confirmation of the final hardware state.

## Graphs, reply format, and locale

Graph queries require the **corresponding lower DUI display to be active**. Discover the appropriate display-selection control and available enum labels instead of guessing a visibility switch or a display label:

```json
{"name":"digilock_commands","arguments":{"target":"dui","filter":"display"}}
```

```json
{"name":"digilock_range","arguments":{"target":"dui","port":60007,"command":"display:view"}}
```

```json
{"name":"digilock_help","arguments":{"target":"dui","port":60007,"command":"display:view"}}
```

Select the appropriate view in the Windows DUI, or use `digilock_set` on `display:view` with the exact firmware-provided enum label **only with write opt-in and operator approval**. Confirm sampling is appropriate (`display:sampling`); hidden DUIs can have sampling disabled. Then, for an active scope display:

```json
{"name":"digilock_query","arguments":{"target":"dui","port":60007,"command":"scope:graph"}}
```

RCI execution tools return structured fields `target`, `host`, `port`, `command`, `response`, and `value`:

- `command` is the wire command, including the suffix or assignment added by the tool.
- `response` is reply text with the prompt, one exact first-line command echo, and one final line ending removed. Other whitespace and internal line endings are preserved.
- `value` is a string extracted only when a query reply starts with the exact `name=` prefix; otherwise it is `null`, including for sets. Multiline values remain strings.
- Arrays, graph data, numeric values, booleans, enum ranges, and help text are **not parsed or normalized**. The manual does not specify a portable graph encoding; consumers must interpret the actual firmware output. Preserve locale-dependent numeric strings rather than assuming JSON floats or a particular delimiter.
- Validation, transport failures, and recognized RCI `%% Error: ` replies produce an MCP tool error containing an `error` string.

The TCP protocol is prompt-framed using `> ` at the start of a line. A payload line starting with that same sequence is inherently ambiguous. Invalid UTF-8, oversized frames, or detected unexpected data after a prompt are rejected; do not assume arbitrary binary graph payloads are supported.

## Static catalog versus live firmware

[`docs/commands.json`](docs/commands.json) is compiled into the binary. Its 281 entries (17 DMS and 264 DUI) preserve manual command names, query/set access, data types, and descriptions. `digilock_commands` is an **offline search**, not a query of the connected firmware; changing the file requires rebuilding the binary.

The manual has ambiguities and duplicate entries. For example, DUI `visibility:lock` appears twice with different descriptions. The catalog preserves both rather than inventing corrected command names; command validation uses the first matching entry. Commands with the same name on DMS and DUI are distinguished by `target`.

Use `digilock_query` with `command: "commandlist"` on the relevant endpoint for **live firmware/software command discovery**, and range/help for actual accepted values. Firmware may differ from the manual. In read-only mode, ordinary queries must be catalogued and marked queryable; range/help queries must reference a catalogued control, including set-only controls. Unknown controls require write opt-in even if their names appear in a live command list. Discovery does not automatically extend the allowlist.

`digilock_raw` is the escape hatch for firmware-specific syntax. For a documented query:

```json
{"name":"digilock_raw","arguments":{"target":"dui","port":60007,"command":"pid1:proportional?"}}
```

Read-only raw access permits only catalogued queries and catalogued range/help requests. With writes enabled, raw commands bypass catalog access/type checks and can perform arbitrary hardware operations, including assignments that the typed setter would reject. They must still be nonempty and contain no control characters. Treat raw write access as full RCI authority, not a safe restricted API.

## Safety, connections, and failure handling

- **Hardware changes require explicit operator opt-in.** `--allow-writes` enables more than parameter tuning: commands can change physical outputs, enable feedback, connect/disconnect modules, or stop Windows applications. Confirm the intended operation and query range/help first. MCP tool annotations are hints, not a substitute for the enforced write guard or operator judgment. Read-only mode is a catalog-based policy, not a hardware safety interlock.
- **One remote connection per RCI endpoint.** The DMS and each DUI allow only one remote client connection. Close other telnet/automation clients and avoid launching competing wrapper instances against the same endpoint. This wrapper opens connections lazily, keeps at most one session per port, serializes requests on that port, and allows separate ports to progress independently.
- **Plain, unauthenticated TCP.** This wrapper adds no TLS, credentials, or network authentication to the RCI. Use only on a trusted, isolated network with narrowly scoped firewall access. **Do not expose RCI ports to the internet**, and do not give untrusted MCP clients write access.
- **Timeouts and transport failures are uncertain outcomes.** The timeout covers the entire RCI transaction, including time queued behind another request. Failed or cancelled in-flight transport transactions close their session; a later request may establish a new connection. The wrapper does **not retry or replay** commands automatically. A timeout, EOF, cancellation, or framing failure does **not prove a change did not execute**. Never automatically retry a write: inspect/query the state and involve the operator before deciding what to do next.
- **Release a client slot explicitly when needed:**

  ```json
  {"name":"digilock_disconnect","arguments":{"target":"dui","port":60007}}
  ```

  The result has `target`, `port`, and a boolean `disconnected` indicating whether an open TCP session was removed. DMS and other DUI sessions are unaffected. This does **not** set `module:connect=false`, disconnect USB hardware, reset outputs, or stop feedback. A later request reconnects. Process shutdown also releases its TCP sessions, not hardware state.

## Source map

- `src/main.rs`: CLI/environment configuration and stdio MCP startup.
- `src/server.rs`: tool schemas, endpoint routing, catalog access checks, and write policy.
- `src/rci.rs`: persistent TCP sessions, prompt framing, transaction deadlines, and mock transport tests.
- `src/catalog.rs`: embedded catalog loading and command/value validation.
- `tests/stdio.rs`: end-to-end MCP stdio tests with local fake RCI endpoints.
- `docs/DigiLock-RCI_Manual.pdf`: RCI protocol and manual command reference.
- `docs/Digilock_Manual.pdf` and `docs/DigiLock_Documentation.pdf`: additional bundled DigiLock documentation.
