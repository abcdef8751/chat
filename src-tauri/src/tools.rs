//! Tool surface: OpenAI-style tool specs, a host executor (bash/read/write,
//! approval-gated), and native Brave Search (web/local/image/video/news/
//! summarizer) called directly over the Brave REST API. The native client needs
//! only a keychain-stored key + network — no node/`~/.config/opencode` path, so
//! it works on Android too. Read-only → ungated.

use std::collections::HashMap;
use std::sync::Mutex;

use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use serde_json::{json, Value};
use tauri::Manager;

/// Tool names needing per-call user approval (local side effects / local data).
pub const GATED_TOOLS: &[&str] = &["bash", "read_file", "write_file"];
/// Hard cap on consecutive tool rounds before handing back to the user.
pub const MAX_TOOL_ROUNDS: u32 = 8;

pub fn is_gated(name: &str) -> bool {
    GATED_TOOLS.contains(&name)
}

/// A tool call parsed out of the assistant stream.
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Outcome of executing a tool.
#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub(crate) fn ok(content: String) -> Self {
        Self { content, is_error: false }
    }
    pub(crate) fn err(content: String) -> Self {
        Self { content, is_error: true }
    }
}

/// Truncate tool output so a runaway command can't blow up the context.
pub(crate) const MAX_OUTPUT_CHARS: usize = 20_000;

pub(crate) fn truncate(s: String) -> String {
    if s.chars().count() > MAX_OUTPUT_CHARS {
        let mut out: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
        out.push_str("\n…[truncated]");
        out
    } else {
        s
    }
}

/// Build one OpenAI function-tool spec.
fn fn_tool(name: &str, desc: &str, params: Value) -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: name.into(),
            description: Some(desc.into()),
            parameters: Some(params),
            strict: None,
        },
    })
}

fn memory_save_tool() -> ChatCompletionTools {
    fn_tool(
        "save_memory",
        "Append a durable fact to a long-term memory file. Use for stable preferences, \
         identity details, goals, and notable details — not transient chat context. The file \
         is created if it doesn't exist; prefer an existing file (profile.md, preferences.md, \
         goals.md) unless a new topic file is warranted.",
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "The fact to remember, written as a short self-contained statement" },
                "path": {
                    "type": "string",
                    "description": "Memory file to append to, e.g. profile.md or project-x.md. Created if missing."
                }
            },
            "required": ["content", "path"]
        }),
    )
}

fn memory_read_tool() -> ChatCompletionTools {
    fn_tool(
        "read_memory",
        "Read the full contents of a long-term memory file on demand (e.g. profile.md, \
         or any file listed in the memory block).",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Memory file name, e.g. profile.md" }
            },
            "required": ["path"]
        }),
    )
}

fn memory_write_tool() -> ChatCompletionTools {
    fn_tool(
        "write_memory",
        "Replace a long-term memory file's entire contents (creates it if missing). Use \
         to merge, deduplicate, and reorganize memory rather than appending.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Memory file name, e.g. profile.md" },
                "content": { "type": "string", "description": "The full new contents of the file" }
            },
            "required": ["path", "content"]
        }),
    )
}

/// OpenAI `tools` array for a request. When `brave` is true the full native
/// Brave tool surface is appended (`brave_web_search`, `brave_local_search`,
/// `brave_image_search`, `brave_video_search`, `brave_news_search`,
/// `brave_summarizer`). The list is **identical for live and reflection turns**
/// (so the prompt-cache prefix matches); `write_memory` is present in both but
/// refused during live turns by `run_tool_loop`'s `memory_only` mode.
pub fn tool_specs(brave: bool) -> Vec<ChatCompletionTools> {
    let mut tools = vec![
        fn_tool(
            "bash",
            "Run a bash command and return stdout/stderr.",
            json!({
                "type": "object",
                "properties": { "command": { "type": "string", "description": "The bash command to run" } },
                "required": ["command"]
            }),
        ),
        fn_tool(
            "read_file",
            "Read a file's contents as text.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Absolute or relative file path" } },
                "required": ["path"]
            }),
        ),
        fn_tool(
            "write_file",
            "Write text content to a file (creates or overwrites).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to write" },
                    "content": { "type": "string", "description": "Text content to write" }
                },
                "required": ["path", "content"]
            }),
        ),
        memory_save_tool(),
        memory_read_tool(),
        memory_write_tool(),
    ];
    if brave {
        tools.extend(brave_tools());
    }
    tools
}

/// Brave's native tool surface (mirrors the official `brave-search-mcp-server`
/// tool catalog). Each maps to one `api.search.brave.com` endpoint; the agent
/// calls them by name and `BraveSearch::call_tool` routes them.
fn brave_tools() -> Vec<ChatCompletionTools> {
    let count = |desc: &str| -> Value {
        json!({ "type": "number", "description": desc })
    };
    vec![
        fn_tool(
            "brave_web_search",
            "General web search returning web pages, news, discussions, FAQs and video \
             results, with pagination and freshness controls.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "count": count("Number of results (1-20, default 10)"),
                    "offset": count("Pagination offset (max 9)"),
                    "freshness": { "type": "string", "description": "Time filter, e.g. pd, pw, pm, py" },
                    "safesearch": { "type": "string", "description": "off|moderate|strict" }
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "brave_local_search",
            "Find local businesses, restaurants, and services with address, phone and rating.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Local search terms, e.g. 'coffee near me'" },
                    "count": count("Number of results (1-20, default 10)")
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "brave_image_search",
            "Search for images with thumbnails and metadata.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "count": count("Number of results (1-200, default 50)"),
                    "safesearch": { "type": "string", "description": "off|moderate|strict" }
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "brave_video_search",
            "Search for videos with thumbnails and metadata.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "count": count("Number of results (1-20, default 10)"),
                    "freshness": { "type": "string", "description": "Time filter, e.g. pd, pw, pm, py" }
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "brave_news_search",
            "Search for recent news articles with freshness filters.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "count": count("Number of results (1-50, default 20)"),
                    "freshness": { "type": "string", "description": "Time filter, e.g. pd, pw, pm, py" }
                },
                "required": ["query"]
            }),
        ),
        fn_tool(
            "brave_summarizer",
            "Search and synthesize results into a short answer (requires an Answers/AI \
             plan). Obtains the summarizer key and calls the Summarizer API automatically.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query to summarize" },
                    "entity_info": { "type": "boolean", "description": "Include extra entity info in the response" },
                    "inline_references": { "type": "boolean", "description": "Include inline references in the response" }
                },
                "required": ["query"]
            }),
        ),
    ]
}

/// Execute a host tool (bash/read_file/write_file). Errors are returned as
/// `ToolOutput::err` text so the model can self-correct (see the failure contract
/// in AGENTS.md). Execution is **one-shot**: `bash` runs a fresh `bash -c <cmd>`
/// per call (no persistent shell), and on Android everything routes through the
/// user's Termux (see `ANDROID_SHELL.md`).
pub async fn execute_host_tool(call: &ToolCall, shell: &crate::shell::ShellExecutor) -> ToolOutput {
    match call.name.as_str() {
        "bash" => {
            let Some(command) = call.arguments.get("command").and_then(Value::as_str) else {
                return ToolOutput::err("bash: missing 'command' argument".into());
            };
            shell.run(command).await
        }
        "read_file" => {
            let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
                return ToolOutput::err("read_file: missing 'path' argument".into());
            };
            shell.read_file(path).await
        }
        "write_file" => {
            let (Some(path), Some(content)) = (
                call.arguments.get("path").and_then(Value::as_str),
                call.arguments.get("content").and_then(Value::as_str),
            ) else {
                return ToolOutput::err("write_file: missing 'path' or 'content' argument".into());
            };
            shell.write_file(path, content).await
        }
        other => ToolOutput::err(format!("unknown tool: {other}")),
    }
}

/// Whether the Brave Search API key is stored, i.e. the native Brave tools can
/// be offered to the model. Reads the OS keychain — no filesystem/env needed,
/// so it works identically on desktop and Android.
pub fn brave_available() -> bool {
    crate::secrets::has_brave_key()
}

/// Native Brave Search client. Calls `api.search.brave.com` directly over
/// `reqwest` (no node/`~/.config/opencode` dependency), so it works on any
/// platform that can reach the network and holds a keychain-stored key.
pub struct BraveSearch {
    http: reqwest::Client,
}

impl Default for BraveSearch {
    fn default() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }
}

const BRAVE_BASE: &str = "https://api.search.brave.com/res/v1";

impl BraveSearch {
    /// Route a model-requested `brave_*` tool to its REST endpoint.
    pub async fn call_tool(&self, name: &str, args: &Value) -> ToolOutput {
        match name {
            "brave_web_search" => self.page_search("web/search", args, false).await,
            "brave_local_search" => self.page_search("web/search", args, true).await,
            "brave_image_search" => self.page_search("images/search", args, false).await,
            "brave_video_search" => self.page_search("videos/search", args, false).await,
            "brave_news_search" => self.page_search("news/search", args, false).await,
            "brave_summarizer" => self.summarizer(args).await,
            other => ToolOutput::err(format!("unknown brave tool: {other}")),
        }
    }

    /// Core GET: build headers + a fixed param list, return parsed JSON.
    async fn get_raw(&self, endpoint: &str, params: &[(&str, String)]) -> Result<Value, String> {
        let key = crate::secrets::get_brave_key()?.ok_or("Brave API key not set")?;
        let url = format!("{BRAVE_BASE}/{endpoint}");
        let mut req = self
            .http
            .get(&url)
            .header("X-Subscription-Token", key)
            .header("Accept", "application/json");
        for (k, v) in params {
            req = req.query(&[(*k, v.as_str())]);
        }
        let resp = req.send().await.map_err(|e| format!("brave: {e}"))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| format!("brave: {e}"))?;
        if !status.is_success() {
            let body: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
            // A missing/expired key (or a Pro-only feature) is the common non-2xx;
            // keep the message short so the model can self-correct with a hint.
            return Err(format!("brave: HTTP {status}: {body}"));
        }
        serde_json::from_slice(&bytes).map_err(|e| format!("brave: bad response: {e}"))
    }

    /// GET a paginated endpoint, deriving query params from the tool args.
    async fn get(
        &self,
        endpoint: &str,
        args: &Value,
        extra: &[(&str, String)],
    ) -> Result<Value, String> {
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(q) = args.get("query").and_then(Value::as_str) {
            params.push(("q".into(), q.to_string()));
        }
        if let Some(c) = args.get("count").and_then(Value::as_u64) {
            params.push(("count".into(), c.to_string()));
        }
        for (k, v) in extra {
            params.push(((*k).into(), v.clone()));
        }
        if let Some(f) = args.get("freshness").and_then(Value::as_str) {
            params.push(("freshness".into(), f.to_string()));
        }
        if let Some(s) = args.get("safesearch").and_then(Value::as_str) {
            params.push(("safesearch".into(), s.to_string()));
        }
        let owned: Vec<(&str, String)> =
            params.iter().map(|p| (p.0.as_str(), p.1.clone())).collect();
        self.get_raw(endpoint, &owned).await
    }

    /// A paginated results endpoint. Local search runs against `web/search`
    /// with `result_filter=locations` (the modern local-discovery path) and
    /// formats the `locations.results` array.
    async fn page_search(&self, endpoint: &str, args: &Value, local: bool) -> ToolOutput {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolOutput::err("missing 'query' argument".into());
        };
        if query.trim().is_empty() {
            return ToolOutput::err("'query' must not be empty".into());
        }
        let extra: &[(&str, String)] = if local {
            &[("result_filter", "locations".into())]
        } else {
            &[]
        };
        match self.get(endpoint, args, extra).await {
            Ok(json) => ToolOutput::ok(truncate(format_results(endpoint, &json))),
            Err(e) => ToolOutput::err(e),
        }
    }

    /// `brave_summarizer` — the Summarizer API (Pro / Answers plan) requires a
    /// one-time `key` that is only issued by a web search with `summary=1`, so
    /// this tool does that handshake itself: (1) `web/search?summary=1` to get
    /// the key, (2) `summarizer/search?key=` to fetch the synthesized answer.
    /// Reads `enrichments.raw` + `enrichments.sources`.
    async fn summarizer(&self, args: &Value) -> ToolOutput {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolOutput::err("missing 'query' argument".into());
        };
        if query.trim().is_empty() {
            return ToolOutput::err("'query' must not be empty".into());
        }
        let web = match self
            .get_raw("web/search", &[("q", query.into()), ("summary", "1".into())])
            .await
        {
            Ok(j) => j,
            Err(e) => return ToolOutput::err(e),
        };
        let Some(key) = web.get("summarizer").and_then(|s| s.get("key")).and_then(Value::as_str) else {
            return ToolOutput::err(
                "brave_summarizer: plan does not support answers (no summarizer key from web/search)".into(),
            );
        };
        let mut params: Vec<(String, String)> =
            vec![("key".into(), key.to_string())];
        if let Some(ei) = args.get("entity_info").and_then(Value::as_bool) {
            params.push(("entity_info".into(), if ei { "true" } else { "false" }.into()));
        }
        if let Some(ir) = args.get("inline_references").and_then(Value::as_bool) {
            params.push(("inline_references".into(), if ir { "true" } else { "false" }.into()));
        }
        let owned: Vec<(&str, String)> = params.iter().map(|p| (p.0.as_str(), p.1.clone())).collect();
        let json = match self.get_raw("summarizer/search", &owned).await {
            Ok(j) => j,
            Err(e) => return ToolOutput::err(e),
        };
        let mut text = String::new();
        if let Some(raw) = json.get("enrichments").and_then(|e| e.get("raw")).and_then(Value::as_str) {
            text.push_str(raw);
        }
        if let Some(sources) = json.get("enrichments").and_then(|e| e.get("sources")).and_then(Value::as_array) {
            for src in sources {
                let title = src.get("title").and_then(Value::as_str).unwrap_or("");
                let url = src.get("url").and_then(Value::as_str).unwrap_or("");
                if title.is_empty() && url.is_empty() {
                    continue;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(title);
                if !url.is_empty() {
                    text.push_str(&format!("\n  {url}"));
                }
            }
        }
        if text.trim().is_empty() {
            text = "No summary returned.".to_string();
        }
        ToolOutput::ok(truncate(text))
    }
}

/// Render a Brave endpoint's result arrays as readable text (title — url,
/// then description). Tolerant of varied response shapes per endpoint.
fn format_results(endpoint: &str, json: &Value) -> String {
    let mut out = String::new();
    let arrays: &[&str] = match endpoint {
        "web/search" => {
            &["web.results", "locations.results", "news.results", "videos.results", "local.results"]
        }
        // news/images/videos/summarizer surface their items under "results".
        _ => &["results", "web.results", "locations.results", "local.results"],
    };
    for path in arrays {
        let mut items: &Value = json;
        let mut missing = false;
        for part in path.split('.') {
            match items.get(part) {
                Some(v) => items = v,
                None => {
                    missing = true;
                    break;
                }
            }
        }
        if missing || !items.is_array() {
            continue;
        }
        for item in items.as_array().unwrap_or(&vec![]) {
            let title = item.get("title").and_then(Value::as_str).unwrap_or("");
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            let desc = item
                .get("description")
                .or_else(|| item.get("snippet"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !out.is_empty() {
                out.push('\n');
            }
            if !title.is_empty() {
                out.push_str(title);
            }
            if !url.is_empty() {
                out.push_str(&format!("\n  {url}"));
            }
            if !desc.is_empty() {
                out.push_str(&format!("\n  {desc}"));
            }
        }
    }
    if out.trim().is_empty() {
        "No results.".to_string()
    } else {
        out
    }
}

/// Pending per-call approvals: the stream emits a `ToolCall` event, then awaits
/// the frontend's approve/deny via a oneshot channel keyed by call id.
#[derive(Default)]
pub struct ApprovalRegistry {
    pending: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
}

impl ApprovalRegistry {
    pub fn register(&self, call_id: String) -> tokio::sync::oneshot::Receiver<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(call_id, tx);
        rx
    }

    pub fn resolve(&self, call_id: &str, approved: bool) -> bool {
        if let Some(tx) = self.pending.lock().unwrap().remove(call_id) {
            let _ = tx.send(approved);
            true
        } else {
            false
        }
    }

    /// Deny everything pending (used on abort/stop).
    pub fn deny_all(&self) {
        let mut map = self.pending.lock().unwrap();
        for (_, tx) in map.drain() {
            let _ = tx.send(false);
        }
    }
}

#[tauri::command]
pub fn approve_tool(
    app: tauri::AppHandle,
    call_id: String,
) -> Result<(), String> {
    let reg = app.state::<ApprovalRegistry>();
    reg.resolve(&call_id, true);
    Ok(())
}

#[tauri::command]
pub fn deny_tool(app: tauri::AppHandle, call_id: String) -> Result<(), String> {
    let reg = app.state::<ApprovalRegistry>();
    reg.resolve(&call_id, false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bash_runs_and_reports_output() {
        let shell = crate::shell::ShellExecutor::new();
        let out = execute_host_tool(
            &ToolCall {
                id: "t1".into(),
                name: "bash".into(),
                arguments: json!({"command": "echo hello"}),
            },
            &shell,
        )
        .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "hello");
    }

    #[tokio::test]
    async fn bash_error_is_tool_error_not_panic() {
        let shell = crate::shell::ShellExecutor::new();
        let out = execute_host_tool(
            &ToolCall {
                id: "t2".into(),
                name: "bash".into(),
                arguments: json!({"command": "(exit 3)"}),
            },
            &shell,
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("exit: 3"));
    }

    #[tokio::test]
    async fn write_and_read_file_roundtrip() {
        let shell = crate::shell::ShellExecutor::new();
        let path = std::env::temp_dir().join(format!("pi-chat-tool-test-{}.txt", std::process::id()));
        let w = execute_host_tool(
            &ToolCall {
                id: "t3".into(),
                name: "write_file".into(),
                arguments: json!({"path": path.to_str().unwrap(), "content": "data"}),
            },
            &shell,
        )
        .await;
        assert!(!w.is_error, "{}", w.content);

        let r = execute_host_tool(
            &ToolCall {
                id: "t4".into(),
                name: "read_file".into(),
                arguments: json!({"path": path.to_str().unwrap()}),
            },
            &shell,
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(r.content, "data");
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn missing_args_become_tool_errors() {
        let shell = crate::shell::ShellExecutor::new();
        let out = execute_host_tool(
            &ToolCall {
                id: "t5".into(),
                name: "read_file".into(),
                arguments: json!({}),
            },
            &shell,
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("missing"));
    }

    #[test]
    fn approval_registry_resolves_once() {
        let reg = ApprovalRegistry::default();
        let rx = reg.register("call-1".into());
        assert!(reg.resolve("call-1", true));
        assert!(!reg.resolve("call-1", true)); // already resolved
        assert!(rx.blocking_recv().unwrap());
    }

    #[test]
    fn tool_specs_shape() {
        let with = tool_specs(true);
        // 6 host/memory tools + 6 native Brave tools.
        assert_eq!(with.len(), 12);
        let without = tool_specs(false);
        assert_eq!(without.len(), 6);
        // The list is identical for live and reflection (cache alignment);
        // `write_memory` is present but gated by mode at execution time.
        assert!(without.iter().any(|t| tool_name(t) == "write_memory"));
        assert!(is_gated("bash"));
        assert!(!is_gated("brave_web_search"));
        assert!(!is_gated("save_memory"));
        assert!(!is_gated("read_memory"));
        assert!(!is_gated("write_memory"));
        // Brave's full tool surface is exposed when a key is configured.
        for name in [
            "brave_web_search",
            "brave_local_search",
            "brave_image_search",
            "brave_video_search",
            "brave_news_search",
            "brave_summarizer",
        ] {
            assert!(with.iter().any(|t| tool_name(t) == name), "missing {name}");
            assert!(!is_gated(name), "{name} should be ungated");
        }
    }

    fn tool_name(t: &ChatCompletionTools) -> &str {
        match t {
            ChatCompletionTools::Function(f) => &f.function.name,
            ChatCompletionTools::Custom(_) => "",
        }
    }

    #[test]
    fn format_results_is_tolerant_and_handles_empty() {
        // Nested web results render as title — url — description.
        let json = json!({
            "web": { "results": [{ "title": "T", "url": "https://x", "description": "D" }] },
            "query": { "original": "q" }
        });
        let text = format_results("web/search", &json);
        assert!(text.contains("T"));
        assert!(text.contains("https://x"));
        assert!(text.contains("D"));

        // News-style responses put items under top-level "results".
        let news = json!({ "results": [{ "title": "N", "url": "https://n" }] });
        assert!(format_results("news/search", &news).contains("https://n"));

        // No results → friendly message, not an error.
        assert_eq!(format_results("web/search", &json!({})), "No results.");
    }

    #[test]
    fn tool_routing_unknown_brave_tool_is_error() {
        let blk = tokio::runtime::Runtime::new().unwrap();
        blk.block_on(async {
            let brave = crate::tools::BraveSearch::default();
            let out = brave
                .call_tool("brave_nonexistent", &json!({}))
                .await;
            assert!(out.is_error);
            assert!(out.content.contains("unknown brave tool"));
        });
    }

    #[test]
    fn brave_available_reflects_keychain() {
        // Without a key the flag is false; the native client is still
        // constructible and degrades to a clear key-not-set error.
        let blk = tokio::runtime::Runtime::new().unwrap();
        blk.block_on(async {
            let brave = crate::tools::BraveSearch::default();
            let out = brave.call_tool("brave_web_search", &json!({"query": "x"})).await;
            if crate::secrets::has_brave_key() {
                // Key present: real network call — assert it doesn't panic and is
                // a ToolOutput (no crash on either an OK or an HTTP error).
                assert!(out.content.contains("No results") || out.is_error || !out.content.is_empty());
            } else {
                assert!(out.is_error);
                assert!(out.content.contains("Brave API key not set"));
            }
        });
    }
}
