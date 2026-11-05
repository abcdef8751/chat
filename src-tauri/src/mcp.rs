//! MCP (Model Context Protocol) client support.
//!
//! Servers are configured in `<app data>/mcp.json`. Each enabled server is
//! asked for its tools (`initialize` → `tools/list`); those tools are appended
//! to the ordinary tool list and dispatched through [`call_tool`].
//!
//! The module is intentionally dependency-free: JSON-RPC 2.0 is spoken over
//! `reqwest` for HTTP servers and `tokio::process` for stdio servers.
//!
//! ## Frozen wire contract (the chat loop and the UI code against this)
//!
//! * `McpServerConfig` / `McpServerStatus` / `McpStatus` / `McpSnapshot` are
//!   camelCase over IPC.
//! * A discovered tool is advertised to the model as
//!   `mcp__<server_id>__<tool_name>` (sanitized). [`is_mcp_tool`] recognizes the
//!   prefix; MCP tool calls are approval-gated by `tools::is_gated`.
//! * [`tool_specs`](McpState::tool_specs) is synchronous and returns the cached
//!   discovery result, so `chat::chat_tools()` stays a plain function.
//! * [`call_tool`](McpState::call_tool) is async and returns a
//!   `tools::ToolOutput`.
//!
//! Discovery is best-effort: a server that is down, misconfigured or speaking
//! nonsense is reported in [`McpServerStatus::error`] and simply contributes no
//! tools. Nothing here can panic or fail app startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_openai::types::chat::ChatCompletionTools;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::Command;
use tokio_stream::StreamExt;

use crate::tools::ToolOutput;

/// Protocol revision this client speaks. `2024-11-05` is the widely supported
/// revision; the newer `2025-03-26` is accepted by current servers too.
const PROTOCOL_VERSION: &str = "2024-11-05";
const CLIENT_NAME: &str = "pi-chat";
/// Tool discovery is quick; a slow server must not stall startup or Settings.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// One tool call may legitimately do real work.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// OpenAI tool names are capped at 64 characters.
const MAX_TOOL_NAME: usize = 64;

/// One configured MCP server. `transport` is `"http"` (streamable HTTP) or
/// `"stdio"` (spawn `command` with `args`/`env`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct McpServerConfig {
    /// Stable id (uuid). Used in the exposed tool name.
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// `http` or `stdio`.
    pub transport: String,
    /// Base URL for `http` servers (e.g. `https://host/mcp`).
    pub url: String,
    /// Executable for `stdio` servers.
    pub command: String,
    pub args: Vec<String>,
    /// Extra environment variables for `stdio` servers.
    pub env: HashMap<String, String>,
    /// Extra request headers for `http` servers (e.g. `Authorization`). A
    /// protocol-critical name is ignored; see [`is_protected_header`].
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            // A freshly created server is enabled unless the caller says
            // otherwise; this also makes an `enabled` key omitted from
            // `mcp.json` mean "on".
            enabled: true,
            transport: "http".into(),
            url: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            headers: HashMap::new(),
        }
    }
}

impl McpServerConfig {
    fn normalized(&self) -> Self {
        let mut config = self.clone();
        let transport = config.transport.trim().to_ascii_lowercase();
        config.transport = if transport.is_empty() {
            "http".into()
        } else {
            transport
        };
        config
    }
}

/// Per-server runtime status for the Settings panel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
    pub tools: usize,
    pub error: Option<String>,
}

/// Aggregate status.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub servers: Vec<McpServerStatus>,
    pub tools: usize,
}

/// What the Settings panel and the chat wiring need in one round trip.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSnapshot {
    pub servers: Vec<McpServerConfig>,
    pub status: McpStatus,
}

/// A discovered tool: which server owns it and its original MCP name.
#[derive(Clone)]
struct McpToolEntry {
    server: McpServerConfig,
    tool_name: String,
}

#[derive(Default)]
struct McpInner {
    configs: Vec<McpServerConfig>,
    tools: Vec<ChatCompletionTools>,
    /// Wire name → server + original tool name.
    map: HashMap<String, McpToolEntry>,
    status: McpStatus,
}

impl McpInner {
    /// Add one server's `tools/list` entries. Returns how many tools were added.
    fn add_discovered(&mut self, config: &McpServerConfig, tools: &[Value]) -> usize {
        let mut added = 0;
        for tool in tools {
            let Some(tool_name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let wire = wire_name(&config.id, tool_name);
            if self.map.contains_key(&wire) {
                eprintln!(
                    "mcp: {} ({}) exposes a duplicate tool name {wire}; skipped",
                    config.name, config.id
                );
                continue;
            }
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            // One malformed schema would be sent in EVERY request (live and
            // reflection) and most providers reject the whole turn with a 400.
            // Accept only an object schema; anything else gets the empty object.
            let schema = match tool.get("inputSchema") {
                Some(v) if v.is_object() => v.clone(),
                _ => json!({ "type": "object", "properties": {} }),
            };
            self.tools
                .push(crate::tools::fn_tool(&wire, description, schema));
            self.map.insert(
                wire,
                McpToolEntry {
                    server: config.clone(),
                    tool_name: tool_name.to_string(),
                },
            );
            added += 1;
        }
        added
    }
}

#[derive(Default)]
pub struct McpState {
    dir: Mutex<Option<PathBuf>>,
    inner: Mutex<McpInner>,
    /// Serializes discovery passes. Startup discovery and a Settings save can
    /// otherwise finish out of order and publish an older tool list.
    reload_lock: tokio::sync::Mutex<()>,
}

/// A misbehaving server must not be able to exhaust memory or blow the model's
/// context window with one response.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ERROR_DETAIL_CHARS: usize = 2_000;

/// Process-wide handle. The tool loop has no `AppHandle`, so discovery results
/// are published here once and read synchronously while building a request.
static GLOBAL: std::sync::OnceLock<McpState> = std::sync::OnceLock::new();

pub fn global() -> &'static McpState {
    GLOBAL.get_or_init(McpState::default)
}

/// Point the module at `<app data>/mcp.json`. Called once from `setup`.
pub fn init(dir: PathBuf) {
    *global().dir.lock().unwrap() = Some(dir);
    // Discovery is network (or subprocess) I/O and must never delay startup, so
    // publish the location and refresh the cache in the background. `snapshot()`
    // keeps working immediately (it reads the config file locally).
    tauri::async_runtime::spawn(async {
        global().reload().await;
    });
}

impl McpState {
    fn config_path(&self) -> Option<PathBuf> {
        self.dir.lock().unwrap().clone().map(|d| d.join("mcp.json"))
    }

    /// Load `mcp.json` and refresh discovery. Best-effort: a broken server is
    /// reported in its status, never a startup failure.
    pub async fn reload(&self) -> McpSnapshot {
        let Some(path) = self.config_path() else {
            return McpSnapshot::default();
        };
        // One discovery pass at a time, and the config is re-read *after* the
        // lock: a save that lands while an older pass is running is therefore
        // picked up by the next pass rather than being overwritten by it.
        let _guard = self.reload_lock.lock().await;
        let configs = read_configs(&path);
        let mut inner = McpInner {
            configs: configs.clone(),
            ..McpInner::default()
        };
        let mut statuses = Vec::with_capacity(configs.len());
        for config in &configs {
            let mut status = McpServerStatus {
                id: config.id.clone(),
                name: config.name.clone(),
                enabled: config.enabled,
                connected: false,
                tools: 0,
                error: None,
            };
            if config.enabled {
                let config = config.normalized();
                match discover(&config).await {
                    Ok(tools) => {
                        let count = inner.add_discovered(&config, &tools);
                        status.connected = true;
                        status.tools = count;
                    }
                    Err(e) => {
                        eprintln!(
                            "mcp: server {} ({}) unavailable: {e}",
                            config.name, config.id
                        );
                        status.error = Some(e);
                    }
                }
            }
            statuses.push(status);
        }
        let status = McpStatus {
            tools: inner.tools.len(),
            servers: statuses,
        };
        let snapshot = McpSnapshot {
            servers: configs,
            status: status.clone(),
        };
        inner.status = status;
        *self.inner.lock().unwrap() = inner;
        snapshot
    }

    /// The last discovery result plus the configured servers. Never touches the
    /// network; before the first reload it still lists the config file so the
    /// Settings panel is not blank.
    pub fn snapshot(&self) -> McpSnapshot {
        let inner = self.inner.lock().unwrap();
        if !inner.configs.is_empty() {
            return McpSnapshot {
                servers: inner.configs.clone(),
                status: inner.status.clone(),
            };
        }
        let servers = self
            .config_path()
            .map(|p| read_configs(&p))
            .unwrap_or_default();
        McpSnapshot {
            servers,
            status: inner.status.clone(),
        }
    }

    /// Cached tool specs from the last discovery pass.
    pub fn tool_specs(&self) -> Vec<ChatCompletionTools> {
        self.inner.lock().unwrap().tools.clone()
    }

    /// Dispatch an `mcp__...` call. Returns an error output (never panics) when
    /// the server is unknown or unreachable.
    pub async fn call_tool(&self, name: &str, arguments: &Value) -> ToolOutput {
        let entry = match self.inner.lock().unwrap().map.get(name) {
            Some(entry) => entry.clone(),
            None => return ToolOutput::err(format!("MCP tool {name} is not available")),
        };
        let params = json!({
            "name": entry.tool_name,
            "arguments": if arguments.is_null() { json!({}) } else { arguments.clone() },
        });
        let result = if entry.server.normalized().transport == "stdio" {
            stdio_call(&entry.server, "tools/call", params, CALL_TIMEOUT).await
        } else {
            http_call(&entry.server, "tools/call", params, CALL_TIMEOUT).await
        };
        match result {
            Ok(result) => flatten_result(&result),
            Err(e) => ToolOutput::err(format!("MCP tool {name} failed: {e}")),
        }
    }

    async fn save_server(&self, mut server: McpServerConfig) -> Result<McpSnapshot, String> {
        let path = self
            .config_path()
            .ok_or_else(|| "MCP directory is not configured".to_string())?;
        if server.id.trim().is_empty() {
            server.id = uuid::Uuid::new_v4().to_string();
        }
        server.transport = server.normalized().transport;
        let mut configs = read_configs(&path);
        match configs.iter_mut().find(|c| c.id == server.id) {
            Some(slot) => *slot = server,
            None => configs.push(server),
        }
        write_configs(&path, &configs)?;
        Ok(self.reload().await)
    }

    async fn remove_server(&self, id: &str) -> Result<McpSnapshot, String> {
        let path = self
            .config_path()
            .ok_or_else(|| "MCP directory is not configured".to_string())?;
        let mut configs = read_configs(&path);
        configs.retain(|c| c.id != id);
        write_configs(&path, &configs)?;
        Ok(self.reload().await)
    }
}

/// True for names exposed by an MCP server.
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with("mcp__")
}

/// Sanitize one name component to the characters OpenAI accepts.
fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The wire name for a discovered tool: `mcp__<server>__<tool>`, sanitized and
/// capped at 64 characters (OpenAI's limit). ASCII-only after sanitizing, so a
/// byte truncation cannot split a character.
fn wire_name(server_id: &str, tool_name: &str) -> String {
    let mut name = format!(
        "mcp__{}__{}",
        sanitize_component(server_id),
        sanitize_component(tool_name)
    );
    if name.len() > MAX_TOOL_NAME {
        // Two long names must not collapse to the same truncated wire name.
        let hash = fnv1a(&format!("{server_id}\0{tool_name}"));
        let suffix = format!("_{hash:08x}");
        let keep = MAX_TOOL_NAME - suffix.len();
        name.truncate(keep);
        name.push_str(&suffix);
    }
    name
}

/// FNV-1a 64-bit, truncated to 32 bits: enough to disambiguate a capped wire
/// name without pulling in a hashing crate.
fn fnv1a(value: &str) -> u32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash ^ (hash >> 32)) as u32
}

/// Flatten a `tools/call` result into one tool output. Text parts are joined;
/// non-text parts (images, resources, …) are compact JSON so the model at least
/// sees what was returned.
fn flatten_result(result: &Value) -> ToolOutput {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for part in content {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                Some("image") => {
                    // Never paste base64 into the prompt: it is unbounded and
                    // useless as text (and can blow the context window).
                    let mime = part
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .unwrap_or("image");
                    parts.push(format!("[image content omitted by pi-chat: {mime}]"));
                }
                _ => {
                    // Other parts (resources/audio) are summarised, not dumped.
                    let kind = part.get("type").and_then(Value::as_str).unwrap_or("part");
                    let preview: String = serde_json::to_string(part)
                        .unwrap_or_default()
                        .chars()
                        .take(2_000)
                        .collect();
                    parts.push(format!("[{kind}] {preview}"));
                }
            }
        }
    }
    // Some servers only return structured content.
    if parts.is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            parts.push(serde_json::to_string(structured).unwrap_or_default());
        }
    }
    let mut content = parts.join("\n");
    if content.trim().is_empty() {
        content = if is_error {
            "MCP tool returned an error.".into()
        } else {
            "MCP tool returned no content.".into()
        };
    }
    // Same ceiling as every other tool: a runaway MCP result must not consume
    // the model's context (or the cost of re-sending it every round).
    let content = crate::tools::truncate(content);
    if is_error {
        ToolOutput::err(content)
    } else {
        ToolOutput::ok(content)
    }
}

// --- Config file ------------------------------------------------------------

fn read_configs(path: &Path) -> Vec<McpServerConfig> {
    if !path.exists() {
        return Vec::new();
    }
    match std::fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Vec::new(),
        Ok(raw) => match serde_json::from_str::<Vec<McpServerConfig>>(&raw) {
            Ok(configs) => configs,
            Err(e) => {
                eprintln!("mcp: {} is not valid JSON: {e}", path.display());
                Vec::new()
            }
        },
        Err(e) => {
            eprintln!("mcp: reading {} failed: {e}", path.display());
            Vec::new()
        }
    }
}

/// Atomic-ish write: a crash mid-write leaves the previous file intact rather
/// than a truncated config.
fn write_configs(path: &Path, configs: &[McpServerConfig]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(configs).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, raw).map_err(|e| format!("writing {} failed: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("replacing {} failed: {e}", path.display()))?;
    Ok(())
}

// --- JSON-RPC transports ----------------------------------------------------

fn jsonrpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown MCP error");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_string(),
    }
}

fn check_jsonrpc_error(message: &Value) -> Result<(), String> {
    match message.get("error") {
        Some(error) if !error.is_null() => Err(jsonrpc_error(error)),
        _ => Ok(()),
    }
}

fn initialize_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": CLIENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
        },
    })
}

fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    })
}

/// Pull one complete `\n\n`-terminated SSE event out of `buf`.
fn take_sse_event(buf: &mut String) -> Option<String> {
    let end = buf.find("\n\n")?;
    let event = buf[..end].to_string();
    buf.drain(..end + 2);
    Some(event)
}

/// Join an SSE event's `data:` lines (MCP sends one JSON-RPC message per event).
fn sse_event_data(event: &str) -> Option<String> {
    let mut data = Vec::new();
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
    }
    if data.is_empty() {
        None
    } else {
        Some(data.join("\n"))
    }
}

/// Read a response body with the shared size ceiling. Used for both success and
/// error bodies: a hostile/hung server must not be able to buffer megabytes just
/// because it returned a non-2xx status.
async fn read_body_capped(response: reqwest::Response) -> Result<String, String> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("MCP response read failed: {e}"))?;
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "MCP response exceeded {} bytes",
                MAX_RESPONSE_BYTES
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Header names this client owns. A user-configured header must never override
/// them: `content-type` and `accept` decide the framing, `mcp-protocol-version`
/// is the negotiated revision, and `mcp-session-id` is assigned by the server.
const PROTECTED_HEADERS: [&str; 4] = [
    "content-type",
    "accept",
    "mcp-protocol-version",
    "mcp-session-id",
];

fn is_protected_header(name: &str) -> bool {
    PROTECTED_HEADERS
        .iter()
        .any(|protected| name.eq_ignore_ascii_case(protected))
}

/// Apply the server's configured headers to a request. Keys are trimmed and a
/// blank key is skipped; values are used verbatim. A protected key is silently
/// ignored, because letting it through would duplicate the header (reqwest
/// appends) and break the handshake. A key/value `reqwest` cannot represent is
/// skipped too instead of panicking (the config file is user-editable).
fn apply_configured_headers(
    mut request: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    use reqwest::header::{HeaderName, HeaderValue};
    for (key, value) in headers {
        let key = key.trim();
        if key.is_empty() || is_protected_header(key) {
            continue;
        }
        let (Ok(name), Ok(value)) = (
            HeaderName::try_from(key),
            HeaderValue::try_from(value.as_str()),
        ) else {
            continue;
        };
        request = request.header(name, value);
    }
    request
}

/// One JSON-RPC POST. Handles both a plain JSON body and a streamable-SSE body
/// (reading events until the reply with the request's id). Returns `None` for
/// a notification (no id) that the server acknowledged without a body.
async fn http_request(
    client: &reqwest::Client,
    url: &str,
    session: &mut Option<String>,
    headers: &HashMap<String, String>,
    payload: &Value,
) -> Result<Option<Value>, String> {
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .json(payload);
    request = apply_configured_headers(request, headers);
    if let Some(id) = session.as_deref() {
        request = request.header("mcp-session-id", id);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("MCP request failed: {e}"))?;
    // The session id is assigned by the server on `initialize` and must ride
    // every later request.
    if let Some(id) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        *session = Some(id.to_string());
    }
    let status = response.status();
    if !status.is_success() {
        let detail: String = match read_body_capped(response).await {
            Ok(body) => body.chars().take(MAX_ERROR_DETAIL_CHARS).collect(),
            // Keep the reason (too large / read failed) instead of showing an
            // empty `MCP HTTP 500: `.
            Err(e) => format!("<error body not shown: {e}>"),
        };
        return Err(format!("MCP HTTP {status}: {detail}"));
    }
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false);
    let want_id = payload.get("id").and_then(Value::as_i64);

    if is_sse {
        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        loop {
            while let Some(event) = take_sse_event(&mut buf) {
                let Some(data) = sse_event_data(&event) else {
                    continue;
                };
                let Ok(message) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                if want_id.is_none() || message.get("id").and_then(Value::as_i64) == want_id {
                    return Ok(Some(message));
                }
            }
            let Some(chunk) = stream.next().await else {
                break;
            };
            let chunk = chunk.map_err(|e| format!("MCP response read failed: {e}"))?;
            // Normalize CRLF so one framing path handles both.
            buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
            if buf.len() > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "MCP response exceeded {} bytes",
                    MAX_RESPONSE_BYTES
                ));
            }
        }
        // A final event without its terminating blank line.
        if let Some(data) = sse_event_data(&buf) {
            if let Ok(message) = serde_json::from_str::<Value>(&data) {
                if want_id.is_none() || message.get("id").and_then(Value::as_i64) == want_id {
                    return Ok(Some(message));
                }
            }
        }
        if want_id.is_none() {
            return Ok(None);
        }
        return Err("MCP SSE response ended without a reply".into());
    }

    let body = read_body_capped(response).await?;
    if body.trim().is_empty() {
        return Ok(None);
    }
    let message: Value =
        serde_json::from_str(&body).map_err(|e| format!("MCP invalid JSON response: {e}"))?;
    Ok(Some(message))
}

/// Perform one MCP session over streamable HTTP: initialize, initialized, then
/// the requested method. Returns its `result`.
async fn http_call(
    config: &McpServerConfig,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let url = config.url.trim();
    if url.is_empty() {
        return Err("MCP server has no URL".into());
    }
    tokio::time::timeout(timeout, async {
        let client = reqwest::Client::new();
        let mut session: Option<String> = None;
        let init = http_request(
            &client,
            url,
            &mut session,
            &config.headers,
            &initialize_request(),
        )
        .await?
        .ok_or_else(|| "MCP initialize returned no response".to_string())?;
        check_jsonrpc_error(&init)?;
        http_request(
            &client,
            url,
            &mut session,
            &config.headers,
            &initialized_notification(),
        )
        .await?;
        let request = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": method,
            "params": params,
        });
        let response = http_request(&client, url, &mut session, &config.headers, &request)
            .await?
            .ok_or_else(|| format!("MCP {method} returned no response"))?;
        check_jsonrpc_error(&response)?;
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    })
    .await
    .map_err(|_| format!("MCP request timed out after {}s", timeout.as_secs()))?
}

async fn write_json_line<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<(), String> {
    let mut line = serde_json::to_string(value).map_err(|e| e.to_string())?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("MCP write failed: {e}"))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("MCP write failed: {e}"))
}

/// Read newline-delimited JSON-RPC until the reply with `id` arrives. Server
/// notifications (no matching id) are skipped.
async fn read_json_response<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut Lines<R>,
    id: i64,
) -> Result<Value, String> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| format!("MCP read failed: {e}"))?
            .ok_or_else(|| "MCP server closed the connection".to_string())?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return Ok(value);
        }
    }
}

/// Perform one MCP session over a stdio server: spawn, initialize, initialized,
/// then the requested method, then kill the child. A per-call process keeps the
/// implementation simple and bounded (no long-lived process to leak).
async fn stdio_call(
    config: &McpServerConfig,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let command = config.command.trim();
    if command.is_empty() {
        return Err("MCP stdio server has no command".into());
    }
    tokio::time::timeout(timeout, async {
        let mut child = Command::new(command)
            .args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("failed to start MCP server {command}: {e}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "MCP server stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "MCP server stdout unavailable".to_string())?;
        let mut lines = BufReader::new(stdout).lines();

        write_json_line(&mut stdin, &initialize_request()).await?;
        let init = read_json_response(&mut lines, 1).await?;
        check_jsonrpc_error(&init)?;
        write_json_line(&mut stdin, &initialized_notification()).await?;
        write_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": method,
                "params": params,
            }),
        )
        .await?;
        let response = read_json_response(&mut lines, 2).await?;
        check_jsonrpc_error(&response)?;
        let result = response.get("result").cloned().unwrap_or(Value::Null);
        let _ = child.kill().await;
        Ok(result)
    })
    .await
    .map_err(|_| format!("MCP stdio request timed out after {}s", timeout.as_secs()))?
}

/// `tools/list` for one enabled server.
async fn discover(config: &McpServerConfig) -> Result<Vec<Value>, String> {
    let result = match config.normalized().transport.as_str() {
        "stdio" => stdio_call(config, "tools/list", json!({}), DISCOVERY_TIMEOUT).await?,
        "http" => http_call(config, "tools/list", json!({}), DISCOVERY_TIMEOUT).await?,
        other => return Err(format!("unknown MCP transport: {other}")),
    };
    Ok(result
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

// --- Tauri commands ---------------------------------------------------------

#[tauri::command]
pub async fn mcp_list_servers() -> Result<McpSnapshot, String> {
    Ok(global().snapshot())
}

#[tauri::command]
pub async fn mcp_save_server(server: McpServerConfig) -> Result<McpSnapshot, String> {
    global().save_server(server).await
}

#[tauri::command]
pub async fn mcp_remove_server(id: String) -> Result<McpSnapshot, String> {
    global().remove_server(&id).await
}

#[tauri::command]
pub async fn mcp_reload() -> Result<McpSnapshot, String> {
    Ok(global().reload().await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn tool_names_are_sanitized_and_capped() {
        assert_eq!(wire_name("srv-1", "do.thing"), "mcp__srv-1__do_thing");
        assert_eq!(
            wire_name("server id", "tool/name:v2"),
            "mcp__server_id__tool_name_v2"
        );
        assert!(is_mcp_tool(&wire_name("s", "t")));
        assert!(!is_mcp_tool("read_file"));

        let long = wire_name("server", &"x".repeat(200));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with("mcp__server__"));
    }

    #[test]
    fn capped_wire_names_stay_unique() {
        let a = wire_name("server", &"a".repeat(120));
        let b = wire_name("server", &"b".repeat(120));
        assert_ne!(a, b, "two long tool names must not collide after capping");
        assert!(a.len() <= MAX_TOOL_NAME && b.len() <= MAX_TOOL_NAME);
    }

    #[test]
    fn image_parts_are_never_inlined_as_base64() {
        let out = flatten_result(&json!({
            "content": [{
                "type": "image",
                "mimeType": "image/png",
                "data": "A".repeat(200_000)
            }]
        }));
        assert!(!out.is_error);
        assert!(out.content.contains("image content omitted"));
        assert!(
            out.content.chars().count() < 1_000,
            "an image part must not be pasted into the prompt"
        );
    }

    #[test]
    fn discovery_maps_wire_names_and_flattens_results() {
        let config = McpServerConfig {
            id: "srv-1".into(),
            name: "Test".into(),
            ..Default::default()
        };
        let mut inner = McpInner::default();
        let tools = vec![json!({
            "name": "echo",
            "description": "Echo text",
            "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } } },
        })];
        assert_eq!(inner.add_discovered(&config, &tools), 1);
        let entry = inner.map.get("mcp__srv-1__echo").unwrap();
        assert_eq!(entry.tool_name, "echo");
        assert_eq!(entry.server.id, "srv-1");
        assert_eq!(inner.tools.len(), 1);

        // A duplicate wire name from another server is skipped, not advertised twice.
        let mut other = config.clone();
        other.id = "srv-1".into();
        assert_eq!(inner.add_discovered(&other, &tools), 0);

        // Two *different* server ids that sanitize to the same component collide
        // on the wire too; the second must be skipped rather than shadowing the
        // first.
        let mut inner2 = McpInner::default();
        let mut a = config.clone();
        a.id = "srv_1".into();
        let mut b = config.clone();
        b.id = "srv 1".into();
        assert_eq!(inner2.add_discovered(&a, &tools), 1);
        assert_eq!(inner2.add_discovered(&b, &tools), 0);

        let ok = flatten_result(&json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "image", "data": "abc", "mimeType": "image/png" },
            ],
        }));
        assert!(!ok.is_error);
        assert!(ok.content.starts_with("hello"));
        // Image parts are summarized, never inlined as base64.
        assert!(ok.content.contains("image content omitted"));
        assert!(ok.content.contains("image/png"));

        let err = flatten_result(&json!({
            "isError": true,
            "content": [{ "type": "text", "text": "boom" }],
        }));
        assert!(err.is_error);
        assert_eq!(err.content, "boom");
    }

    /// Read one HTTP request (headers + content-length body) off the socket.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4) else {
            return String::from_utf8_lossy(&buf).into_owned();
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
            .unwrap_or(0);
        while buf.len() < header_end + len {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn respond(stream: &mut TcpStream, status: &str, body: &str, session: bool) {
        let session_header = if session {
            "mcp-session-id: test-session\r\n"
        } else {
            ""
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{session_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.flush();
    }

    /// Raw HTTP requests the mock has received, in arrival order. Tests use it
    /// to assert on what actually went over the wire.
    type RecordedRequests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// The body of a recorded JSON-RPC request.
    fn request_message(request: &str) -> Value {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body).unwrap_or(Value::Null)
    }

    fn request_method(request: &str) -> String {
        request_message(request)
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// Every value sent for `name` (there should be exactly one).
    fn request_header_values(request: &str, name: &str) -> Vec<String> {
        let head = request.split("\r\n\r\n").next().unwrap_or(request);
        head.lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
            .collect()
    }

    /// A tiny HTTP MCP server: initialize / initialized / tools/list /
    /// tools/call. `requests` is the number of connections it accepts before
    /// the thread exits (a discovery + call sequence is six: one discovery
    /// session plus one call session).
    fn start_mcp_http_mock(requests: usize) -> (u16, JoinHandle<()>, RecordedRequests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let recorded: RecordedRequests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = recorded.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = match listener.accept() {
                    Ok(x) => x,
                    Err(_) => return,
                };
                let request = read_request(&mut stream);
                recorder.lock().unwrap().push(request.clone());
                let message = request_message(&request);
                let method = message.get("method").and_then(Value::as_str).unwrap_or("");
                let id = message.get("id").cloned().unwrap_or(Value::Null);
                match method {
                    "initialize" => {
                        assert!(
                            request.to_ascii_lowercase().contains("accept:"),
                            "client must ask for SSE too"
                        );
                        respond(
                            &mut stream,
                            "200 OK",
                            &json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "protocolVersion": PROTOCOL_VERSION,
                                    "capabilities": {},
                                    "serverInfo": { "name": "mock", "version": "0" },
                                },
                            })
                            .to_string(),
                            true,
                        );
                    }
                    "notifications/initialized" => respond(&mut stream, "202 Accepted", "", false),
                    "tools/list" => {
                        assert!(
                            request.to_ascii_lowercase().contains("mcp-session-id: test-session"),
                            "session id must be resent"
                        );
                        respond(
                            &mut stream,
                            "200 OK",
                            &json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": {
                                    "tools": [{
                                        "name": "echo",
                                        "description": "Echo text",
                                        "inputSchema": { "type": "object", "properties": {} },
                                    }],
                                },
                            })
                            .to_string(),
                            false,
                        );
                    }
                    "tools/call" => respond(
                        &mut stream,
                        "200 OK",
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": "echo: hi" }],
                            },
                        })
                        .to_string(),
                        false,
                    ),
                    other => panic!("unexpected MCP method: {other}"),
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        (port, handle, recorded)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_discovers_http_tools_and_calls_them() {
        let (port, handle, _recorded) = start_mcp_http_mock(6);
        let dir = temp_dir("http");
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::to_string(&vec![McpServerConfig {
                id: "srv-1".into(),
                name: "Mock".into(),
                enabled: true,
                transport: "http".into(),
                url: format!("http://127.0.0.1:{port}/mcp"),
                ..Default::default()
            }])
            .unwrap(),
        )
        .unwrap();

        let state = McpState::default();
        *state.dir.lock().unwrap() = Some(dir.clone());
        let snapshot = state.reload().await;
        assert_eq!(snapshot.servers.len(), 1);
        assert_eq!(snapshot.status.tools, 1);
        let status = &snapshot.status.servers[0];
        assert!(status.connected, "{:?}", status.error);
        assert_eq!(status.tools, 1);
        assert_eq!(state.tool_specs().len(), 1);

        let output = state
            .call_tool("mcp__srv-1__echo", &json!({ "text": "hi" }))
            .await;
        assert!(!output.is_error, "{}", output.content);
        assert_eq!(output.content, "echo: hi");
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_broken_server_is_reported_and_never_panics() {
        let dir = temp_dir("broken");
        std::fs::write(
            dir.join("mcp.json"),
            serde_json::to_string(&vec![McpServerConfig {
                id: "dead".into(),
                name: "Dead".into(),
                enabled: true,
                transport: "http".into(),
                // Port 1 on loopback refuses connections immediately.
                url: "http://127.0.0.1:1/mcp".into(),
                ..Default::default()
            }])
            .unwrap(),
        )
        .unwrap();
        let state = McpState::default();
        *state.dir.lock().unwrap() = Some(dir.clone());
        let snapshot = state.reload().await;
        assert_eq!(snapshot.servers.len(), 1);
        assert_eq!(snapshot.status.tools, 0);
        assert!(!snapshot.status.servers[0].connected);
        assert!(snapshot.status.servers[0].error.is_some());
        assert!(state.tool_specs().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn configured_headers_ride_initialize_and_tools_list() {
        // Two sessions (discovery + one tool call) = six POSTs, so the tool
        // call path is covered too.
        let (port, handle, recorded) = start_mcp_http_mock(6);
        let config = McpServerConfig {
            id: "srv-h".into(),
            name: "Headers".into(),
            enabled: true,
            transport: "http".into(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: HashMap::from([
                ("Authorization".to_string(), "Bearer s3cr3t".to_string()),
                // Keys are trimmed, so this is `X-Vendor-Key`.
                (" X-Vendor-Key ".to_string(), "abc".to_string()),
            ]),
            ..Default::default()
        };

        let result = http_call(&config, "tools/list", json!({}), DISCOVERY_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(result["tools"][0]["name"], "echo");
        let called = http_call(
            &config,
            "tools/call",
            json!({ "name": "echo", "arguments": { "text": "hi" } }),
            CALL_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(called["content"][0]["text"], "echo: hi");
        handle.join().unwrap();

        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 6);
        for method in [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
        ] {
            let requests: Vec<_> = recorded
                .iter()
                .filter(|r| request_method(r) == method)
                .collect();
            assert!(!requests.is_empty(), "no {method} request recorded");
            for request in requests {
                assert_eq!(
                    request_header_values(request, "authorization"),
                    vec!["Bearer s3cr3t"],
                    "{method} must carry the configured Authorization header"
                );
                assert_eq!(
                    request_header_values(request, "x-vendor-key"),
                    vec!["abc"],
                    "{method} must carry the trimmed key"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protocol_headers_cannot_be_overridden() {
        let (port, handle, recorded) = start_mcp_http_mock(3);
        let config = McpServerConfig {
            id: "srv-p".into(),
            name: "Protected".into(),
            enabled: true,
            transport: "http".into(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: HashMap::from([
                ("accept".to_string(), "text/plain".to_string()),
                ("Content-Type".to_string(), "text/html".to_string()),
                ("MCP-Protocol-Version".to_string(), "9999-01-01".to_string()),
                ("Authorization".to_string(), "Bearer ok".to_string()),
                // Not representable as an HTTP header value; must be skipped,
                // not panicked on.
                ("X-Bad".to_string(), "line1\nline2".to_string()),
            ]),
            ..Default::default()
        };

        http_call(&config, "tools/list", json!({}), DISCOVERY_TIMEOUT)
            .await
            .unwrap();
        handle.join().unwrap();

        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 3);
        for request in recorded.iter() {
            // reqwest appends headers, so a second value would mean the user's
            // value got through (and could break the handshake).
            assert_eq!(
                request_header_values(request, "accept"),
                vec!["application/json, text/event-stream"]
            );
            assert_eq!(
                request_header_values(request, "content-type"),
                vec!["application/json"]
            );
            assert_eq!(
                request_header_values(request, "mcp-protocol-version"),
                vec![PROTOCOL_VERSION]
            );
            assert_eq!(
                request_header_values(request, "authorization"),
                vec!["Bearer ok"]
            );
        }
        let all = recorded.join("\n").to_ascii_lowercase();
        assert!(!all.contains("text/plain"), "user accept leaked onto the wire");
        assert!(
            !all.contains("text/html"),
            "user content-type leaked onto the wire"
        );
        assert!(
            !all.contains("9999-01-01"),
            "user protocol version leaked onto the wire"
        );
        assert!(!all.contains("line1"), "an invalid header value must be skipped");
        assert!(is_protected_header("ACCEPT"));
        assert!(!is_protected_header("authorization"));
    }

    #[test]
    fn config_round_trips_headers_and_env_and_legacy_files_still_load() {
        let dir = temp_dir("config");
        let path = dir.join("mcp.json");
        let config = McpServerConfig {
            id: "srv".into(),
            name: "Round trip".into(),
            enabled: true,
            transport: "stdio".into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server".into()],
            env: HashMap::from([("API_KEY".to_string(), "k".to_string())]),
            headers: HashMap::from([("Authorization".to_string(), "Bearer t".to_string())]),
            ..Default::default()
        };
        write_configs(&path, std::slice::from_ref(&config)).unwrap();
        let read = read_configs(&path);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].headers, config.headers);
        assert_eq!(read[0].env, config.env);
        assert_eq!(read[0].args, config.args);
        assert_eq!(read[0].command, "npx");

        // An `mcp.json` written before `headers` existed still parses, and the
        // missing map is empty rather than an error.
        std::fs::write(
            &path,
            r#"[{"id":"legacy","name":"Legacy","transport":"http","url":"http://127.0.0.1:1/mcp"}]"#,
        )
        .unwrap();
        let legacy = read_configs(&path);
        assert_eq!(legacy.len(), 1);
        assert!(legacy[0].headers.is_empty());
        assert!(legacy[0].env.is_empty());
        assert!(legacy[0].enabled, "an omitted `enabled` still means on");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stdio_env_is_passed_to_the_child() {
        let dir = temp_dir("stdio-env");
        let script = dir.join("stdio-mock.sh");
        // A minimal stdio MCP server: answer initialize and tools/list, echoing
        // the injected env var in the discovered tool's description.
        std::fs::write(
            &script,
            r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '{"jsonrpc":"2.0","id":1,"result":{}}\n' ;;
    *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"envcheck","description":"%s"}]}}\n' "$MCP_TEST_TOKEN" ;;
  esac
done
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            id: "srv-env".into(),
            name: "Env".into(),
            enabled: true,
            transport: "stdio".into(),
            command: "sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            env: HashMap::from([("MCP_TEST_TOKEN".to_string(), "s3cr3t-token".to_string())]),
            ..Default::default()
        };

        let tools = discover(&config).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0]["description"], "s3cr3t-token",
            "the child must receive the configured environment"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
