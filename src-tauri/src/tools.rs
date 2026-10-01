//! Tool surface: OpenAI-style tool specs, a host executor (bash/read/write,
//! approval-gated), and `web_search` via a long-lived `rmcp` MCP stdio client
//! (Brave server, read-only → ungated).

use std::collections::HashMap;
use std::sync::Mutex;

use async_openai::types::chat::{ChatCompletionTool, ChatCompletionTools, FunctionObject};
use serde_json::{json, Value};
use tauri::Manager;
use tokio::sync::Mutex as AsyncMutex;

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

/// OpenAI `tools` array for a request. `web_search` is included only when the
/// MCP server is available. The list is **identical for live and reflection
/// turns** (so the prompt-cache prefix matches); `write_memory` is present in
/// both but refused during live turns by `run_tool_loop`'s `memory_only` mode.
pub fn tool_specs(web_search: bool) -> Vec<ChatCompletionTools> {
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
    if web_search {
        tools.push(fn_tool(
            "web_search",
            "Search the web for current information. Returns results with title, URL, and snippet.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "count": { "type": "number", "description": "Number of results (default 5, max 20)" }
                },
                "required": ["query"]
            }),
        ));
    }
    tools
}

/// Execute a host tool (bash/read_file/write_file). Errors are returned as
/// `ToolOutput::err` text so the model can self-correct (see the failure contract
/// in AGENTS.md).
/// `bash` runs in the conversation's persistent shell so `cd`/`export` persist.
pub async fn execute_host_tool(
    call: &ToolCall,
    shell: &crate::shell::ShellRegistry,
    conversation_id: &str,
) -> ToolOutput {
    match call.name.as_str() {
        "bash" => {
            let Some(command) = call.arguments.get("command").and_then(Value::as_str) else {
                return ToolOutput::err("bash: missing 'command' argument".into());
            };
            shell.run(conversation_id, command).await
        }
        "read_file" => {
            let Some(path) = call.arguments.get("path").and_then(Value::as_str) else {
                return ToolOutput::err("read_file: missing 'path' argument".into());
            };
            match tokio::fs::read_to_string(path).await {
                Ok(text) => ToolOutput::ok(truncate(text)),
                Err(e) => ToolOutput::err(format!("read_file: {e}")),
            }
        }
        "write_file" => {
            let (Some(path), Some(content)) = (
                call.arguments.get("path").and_then(Value::as_str),
                call.arguments.get("content").and_then(Value::as_str),
            ) else {
                return ToolOutput::err("write_file: missing 'path' or 'content' argument".into());
            };
            match tokio::fs::write(path, content).await {
                Ok(()) => ToolOutput::ok(format!("wrote {} bytes to {path}", content.len())),
                Err(e) => ToolOutput::err(format!("write_file: {e}")),
            }
        }
        other => ToolOutput::err(format!("unknown tool: {other}")),
    }
}

/// Long-lived `rmcp` client for the Brave search MCP server (stdio).
/// Spawned lazily on first `web_search`; invalidated and re-spawned if the
/// child process dies (a failed call is retried once on a fresh server).
/// The `RunningService` (not just its `Peer`) is kept alive — dropping it
/// closes the transport.
pub struct McpClient {
    script: std::path::PathBuf,
    service: AsyncMutex<Option<rmcp::service::RunningService<rmcp::service::RoleClient, ()>>>,
}

fn brave_script_path() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(
        std::env::var("HOME").unwrap_or_else(|_| "/home/rp".to_string()),
    );
    p.push(".config/opencode/mcp/brave-search.mjs");
    p
}

/// Whether the Brave MCP server script exists (i.e. web_search can be offered).
pub fn brave_script_available() -> bool {
    brave_script_path().exists()
}

impl McpClient {
    pub fn new() -> Self {
        Self {
            script: brave_script_path(),
            service: AsyncMutex::new(None),
        }
    }

    async fn spawn(
        &self,
    ) -> Result<rmcp::service::RunningService<rmcp::service::RoleClient, ()>, String> {
        use tokio::process::Command;
        let mut cmd = Command::new("node");
        cmd.arg(&self.script);
        let transport = rmcp::transport::child_process::TokioChildProcess::new(cmd)
            .map_err(|e| format!("spawn MCP server: {e}"))?;
        // `()` is the no-callback client handler.
        rmcp::service::serve_client((), transport)
            .await
            .map_err(|e| format!("serve MCP client: {e}"))
    }

    /// Ensure a live service exists, returning a cheap-to-clone handle to it.
    async fn get_or_spawn(
        &self,
    ) -> Result<rmcp::service::Peer<rmcp::service::RoleClient>, String> {
        let mut guard = self.service.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn().await?);
        }
        Ok(guard.as_ref().unwrap().peer().clone())
    }

    async fn invalidate(&self) {
        *self.service.lock().await = None;
    }

    /// Run `web_search` through the MCP server, retrying once on a fresh
    /// server if the cached one has died.
    pub async fn web_search(&self, query: &str, count: Option<u64>) -> ToolOutput {
        if !self.script.exists() {
            return ToolOutput::err(
                "web_search: MCP server script not found (~/.config/opencode/mcp/brave-search.mjs)"
                    .into(),
            );
        }
        for attempt in 0..2 {
            let peer = match self.get_or_spawn().await {
                Ok(p) => p,
                Err(e) => return ToolOutput::err(format!("web_search: {e}")),
            };
            let mut arguments = serde_json::Map::new();
            arguments.insert("query".into(), json!(query));
            if let Some(count) = count {
                arguments.insert("count".into(), json!(count));
            }
            let mut params = rmcp::model::CallToolRequestParams::new("web_search");
            params.arguments = Some(arguments);
            match peer.call_tool(params).await {
                Ok(res) => {
                    let mut text = String::new();
                    for content in &res.content {
                        if let rmcp::model::ContentBlock::Text(t) = content {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&t.text);
                        }
                    }
                    return ToolOutput {
                        is_error: res.is_error.unwrap_or(false),
                        content: truncate(text),
                    };
                }
                Err(_e) if attempt == 0 => {
                    self.invalidate().await;
                }
                Err(e) => return ToolOutput::err(format!("web_search: {e}")),
            }
        }
        ToolOutput::err("web_search: unreached".into())
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
        let shell = crate::shell::ShellRegistry::new();
        let out = execute_host_tool(
            &ToolCall {
                id: "t1".into(),
                name: "bash".into(),
                arguments: json!({"command": "echo hello"}),
            },
            &shell,
            "test",
        )
        .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "hello");
    }

    #[tokio::test]
    async fn bash_error_is_tool_error_not_panic() {
        let shell = crate::shell::ShellRegistry::new();
        let out = execute_host_tool(
            &ToolCall {
                id: "t2".into(),
                name: "bash".into(),
                arguments: json!({"command": "(exit 3)"}),
            },
            &shell,
            "test",
        )
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("exit: 3"));
    }

    #[tokio::test]
    async fn write_and_read_file_roundtrip() {
        let shell = crate::shell::ShellRegistry::new();
        let path = std::env::temp_dir().join(format!("pi-chat-tool-test-{}.txt", std::process::id()));
        let w = execute_host_tool(
            &ToolCall {
                id: "t3".into(),
                name: "write_file".into(),
                arguments: json!({"path": path.to_str().unwrap(), "content": "data"}),
            },
            &shell,
            "test",
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
            "test",
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(r.content, "data");
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn missing_args_become_tool_errors() {
        let shell = crate::shell::ShellRegistry::new();
        let out = execute_host_tool(
            &ToolCall {
                id: "t5".into(),
                name: "read_file".into(),
                arguments: json!({}),
            },
            &shell,
            "test",
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
        assert_eq!(with.len(), 7);
        let without = tool_specs(false);
        assert_eq!(without.len(), 6);
        // The list is identical for live and reflection (cache alignment);
        // `write_memory` is present but gated by mode at execution time.
        assert!(without.iter().any(|t| tool_name(t) == "write_memory"));
        assert!(is_gated("bash"));
        assert!(!is_gated("web_search"));
        assert!(!is_gated("save_memory"));
        assert!(!is_gated("read_memory"));
        assert!(!is_gated("write_memory"));
    }

    fn tool_name(t: &ChatCompletionTools) -> &str {
        match t {
            ChatCompletionTools::Function(f) => &f.function.name,
            ChatCompletionTools::Custom(_) => "",
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_server_handshake_lists_web_search() {
        if !brave_script_available() {
            return; // machine without the Brave MCP script — skip
        }
        let client = McpClient::new();
        let peer = client.get_or_spawn().await.expect("spawn MCP server");
        let tools = peer
            .list_tools(Default::default())
            .await
            .expect("list tools");
        assert!(tools.tools.iter().any(|t| t.name == "web_search"));
    }
}
