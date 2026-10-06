# DigiLock MCP wrapper (Rust)

A stdio or Streamable HTTP [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server for the **TOPTICA DigiLock 110 Remote Control Interface (RCI)**. It exposes command discovery, queries, range/help requests, and explicitly opt-in writes to MCP clients.

The implementation follows the bundled [DigiLock 110 RCI manual](docs/DigiLock-RCI_Manual.pdf), **M-038 version 05, February 2023**. This is a TCP wrapper around the Windows DigiLock software, **not a direct USB driver** or a replacement for that software.

```text
MCP client <-- stdio or HTTP /mcp --> digilock-mcp <-- TCP RCI --> Windows DMS / DUI <-- USB --> DigiLock 110
```

- **DMS** (DigiLock Module Server): discovers and manages modules and starts their interfaces.
- **DUI** (DigiLock User Interface): controls one module; each instance has its own RCI TCP port.

## Prerequisites

- Rust **1.88 or newer** and Cargo, as required by `Cargo.toml`.
- A Windows PC running DigiLock software **1.5.4.70 or newer**, with the DigiLock module connected to that PC by USB and the DMS running. The desired DUI must be running before querying its controls; start it in Windows or explicitly opt into DMS writes as described below.
- TCP reachability from the wrapper to the Windows host's RCI ports. The wrapper can run on a different machine; `--host` identifies the **Windows PC**, not the USB device.
- An MCP client that supports launching stdio servers or connecting to Streamable HTTP servers.

The defaults are DMS port `60000` and DUI port `60001`, but discover the actual DUI ports rather than assuming them. Windows firewall rules should allow only the intended trusted clients.

## Build, run, and test

Run these commands from `/workspaces/digilock-mcp`:

```sh
cargo build --release --locked
./target/release/digilock-mcp --help
./target/release/digilock-mcp --host 192.168.1.10
```

Replace `192.168.1.10` with your Windows host's address. This launch is **read-only by default**. Stdio is the default transport (also selectable with `--transport stdio`). The process speaks MCP on stdin/stdout; it is not an interactive RCI command shell and will wait for an MCP client when launched directly. **Stdout belongs exclusively to MCP protocol traffic**; diagnostics go to stderr. Do not inject banners or redirect logs into stdout.

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

Tests exercise CLI defaults, catalog validation, routing/write guards, and TCP framing, timeouts, cancellation, and connection reuse using **local loopback mock servers**. End-to-end tests launch the binary over MCP stdio and Streamable HTTP to check tool discovery, routing, write policy, queued/in-flight MCP cancellation, console command logging, shared RCI connections across HTTP clients, HTTP Host/Origin validation, and SIGINT shutdown. They do not test Windows DigiLock software or physical hardware. Loopback TCP binding and subprocess execution must be permitted in the test environment.

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

### Network mode (Streamable HTTP)

Run the server independently of your MCP client:

```sh
./target/release/digilock-mcp --transport network --listen 127.0.0.1:8080 --host 192.168.1.10
```

Configure a **Streamable HTTP** MCP connection to `http://127.0.0.1:8080/mcp`. Client configuration formats vary; enter that URL in your client's remote-server settings. This is not raw TCP JSON-RPC or the legacy HTTP+SSE transport. Network mode does not read stdin; Ctrl-C stops the listener and active MCP sessions. Multiple MCP clients share the same RCI connections, per-port serialization, and operator write policy. Startup values run once, before any client is served.

**Every RCI command dispatched in network mode is printed to the console on stderr**, including startup assignments, queries, range/help requests, raw commands, and internal safety-check queries. Each line includes the Windows host and actual RCI port:

```text
RCI 192.168.1.10:60001 >> scan:amplitude?
```

A log line records a send attempt, not proof that the device executed it successfully. Policy-rejected commands and commands cancelled while queued are not sent or logged. Stdout remains unused in network mode; stdio mode does not enable this command logging and keeps stdout exclusively for MCP.

The default listen address is loopback. To accept clients on another machine, preferably bind to a specific local interface:

```sh
./target/release/digilock-mcp --transport network --listen 192.168.1.20:8080 --host 192.168.1.10
```

Here `--listen` identifies the **MCP server**, while `--host` identifies the **Windows RCI server**. HTTP Host validation accepts loopback names and the specific bound IP. For a DNS name or a wildcard bind (`0.0.0.0:8080` / `[::]:8080`), explicitly allow the hostname or IP clients use, preferably including the port:

```sh
./target/release/digilock-mcp --transport network --listen 0.0.0.0:8080 --allowed-host 192.168.1.20:8080 --host 192.168.1.10
```

`--allowed-host` can be repeated. Requests carrying a browser `Origin` header are rejected. **The MCP HTTP endpoint has no authentication or TLS**; Host validation is not authentication. Restrict access with a firewall or authenticated secure proxy/tunnel, and do not expose it to the internet. Logs include complete command values, so treat them as operationally sensitive. Writes still require `--allow-writes` and remain subject to the configured policy.

## CLI and environment configuration

Explicit CLI options take precedence over their corresponding environment variables.

| CLI option | Environment variable | Default | Meaning |
| --- | --- | --- | --- |
| `--transport` | `DIGILOCK_TRANSPORT` | `stdio` | MCP transport: `stdio` or `network` (Streamable HTTP at `/mcp`). |
| `--listen` | `DIGILOCK_LISTEN` | `127.0.0.1:8080` | Network MCP bind address (`IP:port`); ignored in stdio mode. Port `0` chooses a free port, reported on stderr. |
| `--allowed-host` | None | Loopback names and the specific bound IP | Additional HTTP Host authority accepted in network mode; repeat as needed. |
| `--host` | `DIGILOCK_HOST` | `127.0.0.1` | Windows host running DMS/DUI; localhost works only when that software is reachable there. |
| `--dms-port` | `DIGILOCK_DMS_PORT` | `60000` | Module Server RCI port. |
| `--dui-port` | `DIGILOCK_DUI_PORT` | `60001` | Default DUI RCI port; a tool request can select another discovered DUI port. |
| `--timeout-ms` | `DIGILOCK_TIMEOUT_MS` | `5000` | Whole RCI transaction deadline in milliseconds, including queue wait, DNS, connect, welcome, send, and reply. |
| `--max-response-bytes` | `DIGILOCK_MAX_RESPONSE_BYTES` | `8388608` (8 MiB) | Limit for each welcome or reply frame, including echo, line endings, and prompt; increase deliberately for large graphs. |
| `--allow-writes` | `DIGILOCK_ALLOW_WRITES` | `false` | Enables hardware writes, subject to any configured write policy; use environment value `true` to opt in. Without a configuration file it also permits arbitrary raw commands. |
| `--config` | `DIGILOCK_CONFIG` | None | Path to an operator JSON configuration containing startup values and a write policy. |

`--help` and `--version` print information and exit. Configuration requires a nonblank host, nonzero and distinct DMS/default DUI ports, a positive timeout, and a response limit of at least two bytes.

## Operator configuration: startup values and write restrictions

After every configuration, launch, firmware, or wiring change, use the [configuration safety-review checklist](docs/safety-config-review.md). It documents known limit-bypass paths, including routing/activation changes, combined signal contributions, relock, and indirect PID parameter updates, with a repeatable review record. Passing configuration parsing or wrapper tests alone does not establish hardware safety.

Load a JSON file with `--config /absolute/path/digilock-config.json` (or `DIGILOCK_CONFIG`). It is read once at process startup; edits require a restart. There is no implicit file discovery or fallback if a supplied file cannot be loaded. CLI/environment endpoint settings remain separate from this file. For example, after reviewing the policy for your apparatus:

```sh
./target/release/digilock-mcp --host 192.168.1.10 --config /absolute/path/digilock-config.json --allow-writes
```

For an MCP client, append `"--config", "/absolute/path/digilock-config.json"` and, only when intended, `"--allow-writes"` to its server `args`.

[`digilock-config.example.json`](digilock-config.example.json) is a starting template with **no startup writes**. Its limits and enum labels are illustrative, **not certified safe values for your laser**. Determine actual safe settings and firmware-supported labels before enabling writes.

For the TA pro saturation-spectroscopy setup (Main In photodiode, Scan Out piezo/feedforward, Main Out Mod DC), [`digilock-config.ta-pro-lock-in.json`](digilock-config.ta-pro-lock-in.json) provides a task-specific allowlist with no startup writes. Its [review and optimization guide](docs/ta-pro-lock-in-config.md) explains the proposed limits, fixed routing, firmware-label checks, and live-state prerequisites. **Manual approval is required before enabling writes; this is not a hardware interlock.**

A populated configuration has this shape:

```json
{
  "write_policy": {
    "mode": "allowlist",
    "commands": [
      { "target": "dui", "command": "pid1:lock:enable" },
      { "target": "dui", "command": "pid1:proportional" },
      { "target": "dui", "command": "pid1:input" }
    ],
    "limits": [
      { "target": "dui", "command": "pid1:lock:enable", "allowed_values": ["false"] },
      { "target": "dui", "command": "pid1:proportional", "min": 0, "max": 1 },
      { "target": "dui", "command": "pid1:input", "allowed_values": ["main in"] }
    ]
  },
  "initial_values": [
    { "target": "dui", "command": "pid1:lock:enable", "value": "false" },
    { "target": "dui", "command": "pid1:proportional", "value": "0,25" }
  ]
}
```

### Write policy

- `write_policy` and its `mode` are required when loading a file. `commands` and `limits` default to empty lists.
- **`allowlist`**: only listed `(target, command)` pairs may be set. An empty list denies all writes.
- **`denylist`**: listed pairs may not be set; other set operations are permitted. An empty list permits all sets, subject to the global write guard, catalog checks, and any limits. Unknown firmware commands can be listed, but their spelling/types cannot be verified offline.
- **Queries, range queries, and help queries are unaffected by this policy**, even for blocked commands or values outside the write limits. Existing protocol/catalog validation remains: without write opt-in, unknown queries are still rejected by the original read-only guard.
- Command entries are control names, without `?` or `=`. Use documented spelling when sending requests. Policy matching conservatively ignores ASCII case and ASCII spaces to prevent spelling variants bypassing restrictions; this may also treat distinct firmware names as equivalent. DMS and DUI remain separate targets. A DUI rule applies to **every DUI port**, including request overrides; changing the port cannot bypass the rule.
- `limits` do **not** grant allowlist membership. For an otherwise permitted command, optional numeric `min` and `max` are inclusive. Either or both may be supplied. Bounds are finite JSON numbers; `min` must not exceed `max`. Known nonnumeric controls cannot have numeric bounds.
- `allowed_values` is a nonempty list of exact RCI value strings, suitable for enums, booleans, or other discrete choices. Case and whitespace matter. If combined with numeric bounds, **both** restrictions apply.
- Constrained numeric strings support a decimal dot or a single decimal comma, plus scientific notation. Comparisons use finite `f64` values; NaN, infinity, whitespace, mixed/repeated separators, units, and other unparseable values are rejected. No thousands separators are supported. The original string is sent unchanged: you must still use the decimal separator expected by Windows/RCI.
- Limits supplement, rather than replace, the device's ranges and boolean/read-only checks. Other than the explicit scan-envelope rule below, they do not account for interactions between parameters or guarantee physical safety.
- With a file loaded, **raw assignments follow exactly the same checks as `digilock_set`**. Raw queries remain unaffected by the write policy. Arbitrary raw syntax that cannot be classified as one query or assignment is rejected, even with `--allow-writes`.

Unknown JSON fields, duplicate command/limit keys (including spelling aliases), invalid constraints, and nonstring startup values are rejected rather than silently ignored. Known read-only controls cannot be listed as writable controls or given write limits. `digilock_info` exposes the loaded write policy and startup entry count offline.

### Combined scan range limit

Optionally add `scan_range` under `write_policy` to constrain the **entire scan envelope**, instead of independently limiting offset and amplitude:

```json
{
  "write_policy": {
    "mode": "allowlist",
    "commands": [
      { "target": "dui", "command": "offset:value" },
      { "target": "dui", "command": "scan:amplitude" }
    ],
    "scan_range": { "min": -1, "max": 1 }
  },
  "initial_values": []
}
```

The illustrative bounds above are in the same units as the RCI values; choose limits appropriate for your apparatus. `min` and `max` are inclusive finite JSON numbers. Either can be omitted, but at least one is required, and `min` must not exceed `max`. Omit `scan_range` (or set it to `null`) to retain the previous behavior with no combined limit.

The rule requires:

```text
min <= offset:value - scan:amplitude / 2
       offset:value + scan:amplitude / 2 <= max
```

Only the specified inequalities are checked for one-sided limits. **Scan amplitude is peak-to-peak**, so the envelope extends by half the amplitude on either side of the offset. The original peak-to-peak value is sent to RCI unchanged.

- Before setting `offset:value`, the wrapper queries `scan:amplitude?` on the same DUI endpoint and combines that live amplitude with the proposed offset.
- Before setting `scan:amplitude`, it queries `offset:value?` and combines that live offset with the proposed amplitude.
- **Every relevant write uses a fresh query.** Neither previous replies nor successful writes or startup values are cached as hardware state. Proposed and live values must parse as finite numbers; amplitude must be nonnegative and the computed endpoints must be finite.
- Missing/malformed replies, query errors/timeouts, or a violated bound prevent the assignment. Existing write opt-in, command policy, and individual limits are still enforced first; `scan_range` does not itself grant permission to write.
- This applies to `digilock_set`, raw assignments (including command spelling aliases), and startup values on every DUI port. Ordinary queries remain unrestricted by the policy.
- Each query/check/write holds one per-port session lock and uses **one whole-transaction timeout**. Other requests through this wrapper cannot interleave between the query and write. A blocked write is not retried or silently clamped.

This is a guard on changes to these two controls, **not continuous monitoring or a hardware interlock**. A local DUI action or independent hardware/software update can still change state after the query. Ensure `offset:output` and `scan:output` select the intended common output; constrain their values with `allowed_values` where appropriate. Changing output selectors, enabling a scan, and other signal contributions are not covered by this rule. Existing unsafe state is not automatically corrected.

### Startup values

`initial_values` defaults to `[]`. Add one entry for each settable parameter you want initialized; no unspecified defaults are sent. Entries use the same `target`, optional `port`, exact `command`, and string `value` fields as `digilock_set`. You may repeat a control deliberately to express an ordered sequence. Query-only controls cannot be initialized. All startup entries must be allowed by the write policy and satisfy its limits.

**Startup values are hardware writes and require `--allow-writes`.** A nonempty startup list without write opt-in is a startup error, not a silently skipped initialization. The configuration and all statically checkable startup values are validated **before any TCP connection**. When `scan_range` is configured, its live counterpart query and combined-range check run immediately before each relevant startup write; future entries are not used to excuse an unsafe intermediate range. Valid entries are then sent sequentially in array order, waiting for each RCI reply, **before MCP initialization or any tool call is accepted**. This happens once per process launch, not on TCP reconnect.

Any device/transport error aborts startup, returns a nonzero exit status, and prevents subsequent entries from being sent. Earlier writes may already have taken effect; a failed or cancelled write may have taken effect too. There is **no automatic retry, rollback, or read-back verification**. Inspect the hardware state before restarting: a new process launch applies the list again. Startup errors and progress go to stderr only.

Protect the configuration and launch arguments from modification by untrusted clients. These checks cover only this wrapper: local DUI controls and other direct RCI clients can still change hardware. They are not a hardware interlock or network authentication layer.

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

Read-only raw access permits only catalogued queries and catalogued range/help requests. Without a configuration file, writes-enabled raw commands retain the legacy escape hatch: they bypass catalog access/type checks and can perform arbitrary hardware operations, including assignments that the typed setter would reject. They must still be nonempty and contain no control characters. Treat raw write access **without a configuration file** as full RCI authority, not a safe restricted API. With a configuration file loaded, raw assignments obey catalog checks and the operator command/value policy, and arbitrary raw syntax is rejected.

## Safety, connections, and failure handling

- **Hardware changes require explicit operator opt-in.** `--allow-writes` enables more than parameter tuning: commands can change physical outputs, enable feedback, connect/disconnect modules, or stop Windows applications. Confirm the intended operation and query range/help first. MCP tool annotations are hints, not a substitute for the enforced write guard or operator judgment. Read-only mode is a catalog-based policy, not a hardware safety interlock.
- **One remote connection per RCI endpoint.** The DMS and each DUI allow only one remote client connection. Close other telnet/automation clients and avoid launching competing wrapper instances against the same endpoint. This wrapper opens connections lazily, keeps at most one session per port, serializes requests on that port, and allows separate ports to progress independently.
- **Unauthenticated networking.** This wrapper adds no TLS, credentials, or network authentication to the RCI or the network MCP endpoint. Use only on a trusted, isolated network with narrowly scoped firewall access. **Do not expose RCI ports to the internet**, and do not give untrusted MCP clients write access.
- **Timeouts and transport failures are uncertain outcomes.** The timeout covers the entire RCI transaction, including time queued behind another request. Failed or cancelled in-flight transport transactions close their session; a later request may establish a new connection. The wrapper does **not retry or replay** commands automatically. A timeout, EOF, cancellation, or framing failure does **not prove a change did not execute**. Never automatically retry a write: inspect/query the state and involve the operator before deciding what to do next.
- **Release a client slot explicitly when needed:**

  ```json
  {"name":"digilock_disconnect","arguments":{"target":"dui","port":60007}}
  ```

  The result has `target`, `port`, and a boolean `disconnected` indicating whether an open TCP session was removed. DMS and other DUI sessions are unaffected. This does **not** set `module:connect=false`, disconnect USB hardware, reset outputs, or stop feedback. A later request reconnects. Process shutdown also releases its TCP sessions, not hardware state.

## Source map

- `src/main.rs`: CLI/environment configuration, transport selection, and stdio MCP startup.
- `src/network.rs`: Streamable HTTP MCP listener, Host/Origin validation, shared sessions, and shutdown.
- `src/server.rs`: tool schemas, endpoint routing, startup writes, and catalog/policy enforcement.
- `src/config.rs`: strict operator JSON configuration loading.
- `src/policy.rs`: allowlist/denylist matching and numeric/discrete value limits.
- `digilock-config.example.json`: illustrative configuration template without startup writes.
- `src/rci.rs`: persistent TCP sessions, prompt framing, transaction deadlines, and mock transport tests.
- `src/catalog.rs`: embedded catalog loading and command/value validation.
- `tests/stdio.rs`: end-to-end MCP stdio tests with local fake RCI endpoints.
- `tests/network.rs`: end-to-end Streamable HTTP, command logging, and shared-client tests with local fake RCI endpoints.
- `docs/DigiLock-RCI_Manual.pdf`: RCI protocol and manual command reference.
- `docs/Digilock_Manual.pdf` and `docs/DigiLock_Documentation.pdf`: additional bundled DigiLock documentation.
