use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_openai::types::chat::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ImageUrl,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::Manager;
use tokio_stream::StreamExt;

use crate::config::{AppConfig, ConfigState};
use crate::db;
use crate::shell::ShellRegistry;
use crate::tools::{self, ToolCall, ToolOutput};

const DEFAULT_SYSTEM_PROMPT: &str =
    r#"You are a helpful general-purpose assistant. Be concise, accurate, and clear."#;

/// Per-conversation cancellation flags so the UI can abort a running stream.
#[derive(Default)]
pub struct StreamRegistry {
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl StreamRegistry {
    pub fn register(&self, id: String) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.flags.lock().unwrap().insert(id, flag.clone());
        flag
    }

    pub fn cancel(&self, id: &str) {
        if let Some(flag) = self.flags.lock().unwrap().get(id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn remove(&self, id: &str) {
        self.flags.lock().unwrap().remove(id);
    }
}

/// Events pushed to the frontend over the stream channel.
///
/// The channel carries one JSON message per event, tagged by `type`.
#[derive(Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum StreamEvent {
    /// A chunk of assistant text.
    Delta { text: String },
    /// A chunk of the model's private reasoning trace (shown behind a toggle).
    ThinkingDelta { text: String },
    /// The model wants to call a tool; gated ones await approve/deny.
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
        gated: bool,
    },
    /// A tool finished; `ok` mirrors the tool-level error flag.
    ToolResult {
        call_id: String,
        name: String,
        ok: bool,
        output: String,
    },
    /// The stream finished normally; `stop_reason` is `stop`/`length`, etc.
    Done { stop_reason: String },
    /// The stream failed mid-turn; partial text is preserved server-side.
    Error { message: String },
}

/// Where stream deltas are delivered. The Tauri command uses the channel; tests
/// can use an in-memory sink to assert on emitted events.
pub trait EventSink: Send + Sync {
    fn emit(&self, ev: StreamEvent);
}

struct ChannelSink<'a>(&'a Channel<StreamEvent>);

impl EventSink for ChannelSink<'_> {
    fn emit(&self, ev: StreamEvent) {
        let _ = self.0.send(ev);
    }
}

/// Everything a single completion round produced.
#[derive(Default)]
struct RoundAccum {
    text: String,
    thinking: String,
    usage: Option<Value>,
    finish: Option<String>,
    failed: bool,
    tool_calls: Vec<ToolCall>,
}

/// Join the `data:` payload lines of one raw SSE event, if it has any.
fn event_payload(event: &str) -> Option<String> {
    let mut data = Vec::new();
    for raw in event.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(payload) = line.strip_prefix("data:") {
            data.push(payload.strip_prefix(' ').unwrap_or(payload).to_string());
        }
    }
    if data.is_empty() {
        None
    } else {
        Some(data.join("\n"))
    }
}

/// Extract one complete SSE event (terminated by a blank line) from the front of
/// `buf`, returning the joined `data:` payload lines. Returns `None` when no
/// complete event is buffered yet (bytes may still be streaming in).
fn take_first_event(buf: &mut Vec<u8>) -> Option<String> {
    let mut delim: Option<(usize, usize)> = None;
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            delim = Some((i, 2));
            break;
        }
        if buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && i + 3 < buf.len()
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            delim = Some((i, 4));
            break;
        }
        i += 1;
    }
    let (i, len) = delim?;
    let event = String::from_utf8_lossy(&buf[..i]).into_owned();
    buf.drain(..i + len);
    event_payload(&event)
}

/// Read a provider error body (`{"error":"..."}` or `{"error":{"message":...}}`)
/// into a readable message.
fn error_message(err: &Value) -> String {
    if let Some(s) = err.as_str() {
        return s.to_string();
    }
    if let Some(msg) = err.get("message").and_then(Value::as_str) {
        return msg.to_string();
    }
    err.to_string()
}

/// Apply one parsed stream chunk to the running accumulation, emitting events.
/// Reads both the classic OpenAI shape (`content` string) and the shapes
/// providers use for thinking models: a `reasoning_content` string on the
/// delta, or `content` arrays whose parts carry `type: "thinking"|"reasoning"`.
/// Returns `true` when the chunk is fatal and the stream must stop consuming.
fn apply_chunk(
    out: &mut RoundAccum,
    calls: &mut HashMap<u32, (Option<String>, Option<String>, String)>,
    chunk: &Value,
    sink: &dyn EventSink,
) -> bool {
    // Some providers stream an error payload and then close; surface it instead
    // of silently truncating the turn.
    if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
        out.failed = true;
        sink.emit(StreamEvent::Error {
            message: error_message(err),
        });
        return true;
    }
    if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
        out.usage = serde_json::to_value(usage).ok();
    }
    let Some(choice) = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return false;
    };
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        if reason == "error" {
            out.failed = true;
            sink.emit(StreamEvent::Error {
                message: "provider error".to_string(),
            });
            return true;
        }
        if !reason.is_empty() {
            out.finish = Some(reason.to_string());
        }
    }
    let delta = &choice["delta"];

    // Classic string content, or a content array (text vs thinking parts).
    match delta.get("content") {
        Some(Value::String(s)) if !s.is_empty() => {
            out.text.push_str(s);
            sink.emit(StreamEvent::Delta { text: s.clone() });
        }
        Some(Value::Array(items)) => {
            for item in items {
                let Some(kind) = item.get("type").and_then(Value::as_str) else {
                    continue;
                };
                match kind {
                    "text" | "output_text" => {
                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                            if !t.is_empty() {
                                out.text.push_str(t);
                                sink.emit(StreamEvent::Delta {
                                    text: t.to_string(),
                                });
                            }
                        }
                    }
                    "thinking" | "reasoning" | "reasoning_content" => {
                        let t = item
                            .get("text")
                            .or_else(|| item.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !t.is_empty() {
                            out.thinking.push_str(t);
                            sink.emit(StreamEvent::ThinkingDelta {
                                text: t.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    // DeepSeek/Qwen-style reasoning, streamed in its own delta field.
    if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
        if !r.is_empty() {
            out.thinking.push_str(r);
            sink.emit(StreamEvent::ThinkingDelta {
                text: r.to_string(),
            });
        }
    }
    // Streamed tool-call fragments, accumulated by index.
    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
            let slot = calls.entry(index).or_insert((None, None, String::new()));
            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                slot.0 = Some(id.to_string());
            }
            if let Some(f) = tc.get("function") {
                if let Some(name) = f.get("name").and_then(Value::as_str) {
                    slot.1 = Some(name.to_string());
                }
                if let Some(args) = f.get("arguments").and_then(Value::as_str) {
                    slot.2.push_str(args);
                }
            }
        }
    }
    false
}

/// Apply one already-extracted SSE event payload. Returns `true` when the stream
/// should stop consuming (the `[DONE]` sentinel or a fatal error).
fn process_event(
    out: &mut RoundAccum,
    calls: &mut HashMap<u32, (Option<String>, Option<String>, String)>,
    event: &str,
    sink: &dyn EventSink,
) -> bool {
    let event = event.trim();
    if event == "[DONE]" {
        return true;
    }
    match serde_json::from_str::<Value>(event) {
        Ok(chunk) => apply_chunk(out, calls, &chunk, sink),
        Err(_) => false,
    }
}

/// POST a streaming chat-completions request to the configured base URL.
/// Returns the response when the server accepted it (2xx); the body is kept
/// open so the caller can stream and parse SSE events from it.
async fn open_completion_stream(
    base_url: &str,
    api_key: &str,
    body: &Value,
) -> Result<reqwest::Response, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("create stream: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("create stream: HTTP {status}: {detail}"));
    }
    Ok(resp)
}

/// Request body for one completion round.
///
/// `reasoning` carries the just-produced reasoning trace for the most recent
/// assistant tool-call message in `messages` (same turn only). async-openai's
/// assistant message type has no `reasoning_content` field, so it is injected
/// into the serialized JSON here. DeepSeek's thinking mode requires it on the
/// assistant tool-call message or the next round fails with HTTP 400; providers
/// that reject unknown fields can pass `None` (see
/// `AppConfig.echo_reasoning_content`).
fn completion_body(
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tools: &[async_openai::types::chat::ChatCompletionTools],
    thinking_level: &str,
    reasoning: Option<&str>,
) -> Result<Value, String> {
    let mut serialized = serde_json::to_value(messages).map_err(|e| e.to_string())?;
    if let Some(text) = reasoning.filter(|t| !t.trim().is_empty()) {
        if let Some(arr) = serialized.as_array_mut() {
            // Attach to the most recent assistant tool-call message; earlier
            // turns' tool-call rows are replayed reasoning-free.
            for msg in arr.iter_mut().rev() {
                if msg.get("role").and_then(Value::as_str) == Some("assistant")
                    && msg.get("tool_calls").is_some()
                {
                    msg["reasoning_content"] = json!(text);
                    break;
                }
            }
        }
    }
    let mut body = json!({
        "model": model,
        "messages": serialized,
        "stream": true,
        "stream_options": { "include_usage": true },
        "tools": serde_json::to_value(tools).map_err(|e| e.to_string())?,
    });
    let level = thinking_level.trim();
    if !level.is_empty() && level != "default" {
        body["reasoning_effort"] = json!(level);
    }
    Ok(body)
}

/// Consume a completion SSE stream, emitting `Delta`/`ThinkingDelta`/`Error`
/// events as chunks arrive, accumulating streamed tool-call fragments, and
/// stopping early when `flag` is set (abort). The stream is read in its raw
/// form (not through async-openai's typed parser) so provider reasoning fields
/// like `reasoning_content` are not dropped.
async fn run_completion(
    response: reqwest::Response,
    sink: &dyn EventSink,
    flag: &AtomicBool,
) -> RoundAccum {
    let mut out = RoundAccum::default();
    let mut calls: HashMap<u32, (Option<String>, Option<String>, String)> = HashMap::new();
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut closed = false;
    // A persistent deadline so the cancel flag is polled even while chunks keep
    // arriving faster than the poll interval.
    let poll = std::time::Duration::from_millis(200);
    let mut deadline = tokio::time::Instant::now() + poll;

    'consume: loop {
        if flag.load(Ordering::SeqCst) {
            break;
        }
        while let Some(event) = take_first_event(&mut buf) {
            if flag.load(Ordering::SeqCst) {
                break 'consume;
            }
            if process_event(&mut out, &mut calls, &event, sink) {
                break 'consume;
            }
        }
        if closed {
            // The body ended without a terminating blank line; flush the final
            // event so a truncated trailing delta is not dropped.
            let residual = String::from_utf8_lossy(&buf).into_owned();
            buf.clear();
            if let Some(payload) = event_payload(&residual) {
                process_event(&mut out, &mut calls, &payload, sink);
            }
            break;
        }
        tokio::select! {
            maybe = stream.next() => {
                match maybe {
                    None => closed = true,
                    Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
                    Some(Err(e)) => {
                        out.failed = true;
                        sink.emit(StreamEvent::Error { message: format!("stream error: {e}") });
                        break;
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                deadline = tokio::time::Instant::now() + poll;
            }
        }
    }

    let mut indices: Vec<_> = calls.keys().copied().collect();
    indices.sort();
    for idx in indices {
        let (id, name, args) = &calls[&idx];
        out.tool_calls.push(ToolCall {
            id: id.clone().unwrap_or_else(|| format!("call_{idx}")),
            name: name.clone().unwrap_or_default(),
            arguments: serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
        });
    }
    out
}

/// Sum streamed usage across tool rounds so the final message carries the
/// whole turn's token counts.
fn sum_usage(total: &mut Value, round: &Option<Value>) {
    let Some(round) = round else { return };
    for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        let add = round.get(key).and_then(Value::as_i64).unwrap_or(0);
        let cur = total.get(key).and_then(Value::as_i64).unwrap_or(0);
        total[key] = json!(cur + add);
    }
    // Normalize provider-specific cached-token counts so the price meter bills
    // them at the cache rate rather than the full input rate.
    let cached = round
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_i64)
        .or_else(|| round.get("cache_read_input_tokens").and_then(Value::as_i64))
        .or_else(|| round.get("prompt_cache_hit_tokens").and_then(Value::as_i64))
        .unwrap_or(0);
    let cache_write = round
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if cached != 0 {
        let cur = total
            .get("cached_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        total["cached_tokens"] = json!(cur + cached);
    }
    if cache_write != 0 {
        let cur = total
            .get("cache_write_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        total["cache_write_tokens"] = json!(cur + cache_write);
    }
}

/// Record the last round's prompt+completion as the conversation's current
/// context size. Usage is summed across tool rounds (for cost), but context is
/// only the final round's prompt plus its output — what gets sent next turn.
fn track_context(total: &mut Value, round: &Option<Value>) {
    let Some(round) = round else { return };
    let prompt = round
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion = round
        .get("completion_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if prompt > 0 || completion > 0 {
        total["context_tokens"] = json!(prompt + completion);
    }
}

/// Content stored for an assistant turn that was a tool-call request:
/// `{"tool_calls":[{"id","name","arguments"}]}`.
fn encode_tool_calls(calls: &[ToolCall]) -> String {
    let arr: Vec<Value> = calls
        .iter()
        .map(|c| json!({"id": c.id, "name": c.name, "arguments": c.arguments}))
        .collect();
    json!({"tool_calls": arr}).to_string()
}

/// Content stored for a tool result message:
/// `{"tool_call_id","name","error","output"}`.
fn encode_tool_result(call_id: &str, name: &str, output: &ToolOutput) -> String {
    json!({
        "tool_call_id": call_id,
        "name": name,
        "error": output.is_error,
        "output": output.content,
    })
    .to_string()
}

/// Build a user request message, folding text-file contents into the text and
/// images into multimodal `image_url` parts (so vision models receive them).
fn user_request_message(
    text: &str,
    attachments: &[crate::attachments::Attachment],
) -> Result<ChatCompletionRequestMessage, String> {
    let mut combined = text.to_string();
    let mut images: Vec<String> = Vec::new();
    for a in attachments {
        if let Some(t) = &a.text {
            if !t.trim().is_empty() {
                if !combined.trim().is_empty() {
                    combined.push_str("\n\n");
                }
                combined.push_str(&format!("--- File: {} ---\n{}", a.name, t));
            }
        }
        if a.kind == "image" {
            if let Some(url) = &a.data_url {
                images.push(url.clone());
            }
        }
    }

    if images.is_empty() {
        return Ok(ChatCompletionRequestUserMessageArgs::default()
            .content(combined)
            .build()
            .map_err(|e| e.to_string())?
            .into());
    }

    let mut parts: Vec<ChatCompletionRequestUserMessageContentPart> = Vec::new();
    if !combined.trim().is_empty() {
        parts.push(ChatCompletionRequestUserMessageContentPart::Text(
            ChatCompletionRequestMessageContentPartText { text: combined },
        ));
    }
    for url in images {
        parts.push(ChatCompletionRequestUserMessageContentPart::ImageUrl(
            ChatCompletionRequestMessageContentPartImage {
                image_url: ImageUrl { url, detail: None },
            },
        ));
    }
    Ok(ChatCompletionRequestUserMessageArgs::default()
        .content(ChatCompletionRequestUserMessageContent::Array(parts))
        .build()
        .map_err(|e| e.to_string())?
        .into())
}

/// Parse the stored attachment JSON on a user row (empty when absent/invalid).
fn row_attachments(row: &db::MessageRow) -> Vec<crate::attachments::Attachment> {
    row.attachments
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// Convert an existing message row and a fresh user turn into request messages.
/// Assistant rows holding a tool-call payload and tool rows holding results
/// are converted back into their request shapes so multi-round history works.
fn build_messages(
    system_prompt: &str,
    history: &[db::MessageRow],
    user_content: &str,
    user_attachments: &[crate::attachments::Attachment],
) -> Result<Vec<ChatCompletionRequestMessage>, String> {
    let mut out = Vec::new();

    if !system_prompt.trim().is_empty() {
        out.push(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system_prompt.to_string())
                .build()
                .map_err(|e| e.to_string())?
                .into(),
        );
    }

    for row in history {
        match row.role.as_str() {
            "user" => out.push(user_request_message(&row.content, &row_attachments(row))?),
            "assistant" => {
                if let Ok(v) = serde_json::from_str::<Value>(&row.content) {
                    if let Some(calls) = v.get("tool_calls").and_then(Value::as_array) {
                        let mut req_calls = Vec::new();
                        for c in calls {
                            req_calls.push(
                                async_openai::types::chat::ChatCompletionMessageToolCalls::Function(
                                    async_openai::types::chat::ChatCompletionMessageToolCall {
                                        id: c
                                            .get("id")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_string(),
                                        function: async_openai::types::chat::FunctionCall {
                                            name: c
                                                .get("name")
                                                .and_then(Value::as_str)
                                                .unwrap_or_default()
                                                .to_string(),
                                            arguments: c
                                                .get("arguments")
                                                .map(|a| a.to_string())
                                                .unwrap_or_else(|| "{}".into()),
                                        },
                                    },
                                ),
                            );
                        }
                        out.push(
                            ChatCompletionRequestAssistantMessageArgs::default()
                                .tool_calls(req_calls)
                                .build()
                                .map_err(|e| e.to_string())?
                                .into(),
                        );
                        continue;
                    }
                }
                out.push(
                    ChatCompletionRequestAssistantMessageArgs::default()
                        .content(row.content.clone())
                        .build()
                        .map_err(|e| e.to_string())?
                        .into(),
                );
            }
            "tool" => {
                if let Ok(v) = serde_json::from_str::<Value>(&row.content) {
                    let call_id = v.get("tool_call_id").and_then(Value::as_str);
                    let output = v.get("output").and_then(Value::as_str);
                    if let (Some(call_id), Some(output)) = (call_id, output) {
                        out.push(
                            ChatCompletionRequestToolMessageArgs::default()
                                .content(output.to_string())
                                .tool_call_id(call_id.to_string())
                                .build()
                                .map_err(|e| e.to_string())?
                                .into(),
                        );
                        continue;
                    }
                }
                // Malformed tool row: pass through as plain tool output.
                out.push(
                    ChatCompletionRequestToolMessageArgs::default()
                        .content(row.content.clone())
                        .tool_call_id("unknown")
                        .build()
                        .map_err(|e| e.to_string())?
                        .into(),
                );
            }
            _ => {}
        }
    }

    out.push(user_request_message(user_content, user_attachments)?);

    Ok(out)
}

/// Base system prompt, with user preferences injected at the top and the
/// active model named, then the bounded memory index (titles + summaries)
/// appended. Memory bodies stay out of context until `read_memory` is called.
fn build_system_prompt(
    base: &str,
    model_label: &str,
    preferences: &str,
    memory: &crate::memory::MemoryState,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !preferences.trim().is_empty() {
        parts.push(format!("User preferences:\n{}", preferences.trim()));
    }
    parts.push(base.trim().to_string());
    if !model_label.trim().is_empty() {
        parts.push(format!(
            "You are currently running as the model \"{}\".",
            model_label.trim()
        ));
    }
    let mut prompt = parts.join("\n\n");
    let block = memory.context_block();
    if !block.trim().is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&block);
    }
    prompt
}

/// The chat loop proper, factored out of the Tauri command so it can be driven
/// by tests with an in-memory DB, a mock SSE endpoint, and an owned registry:
/// persist the user turn, run the capped tool loop, persist each round, and
/// finish by storing the final assistant turn (with usage + reasoning).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_chat_turn(
    db: &db::Db,
    cfg: &AppConfig,
    api_key: &str,
    mcp: &tools::McpClient,
    shell: &ShellRegistry,
    memory: &crate::memory::MemoryState,
    approvals: &tools::ApprovalRegistry,
    flag: Arc<AtomicBool>,
    sink: &dyn EventSink,
    conversation_id: String,
    content: String,
    attachments: Vec<crate::attachments::Attachment>,
) -> Result<(), String> {
    let conversation = db::get_conversation(db, &conversation_id)?
        .ok_or_else(|| format!("conversation not found: {conversation_id}"))?;
    let history = db::read_messages(db, &conversation_id)?;

    // Persist the incoming user turn before streaming so it survives an abort/error.
    let attachments_json = if attachments.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&attachments).map_err(|e| e.to_string())?)
    };
    db::insert_message_full(
        db,
        conversation_id.clone(),
        "user".into(),
        content.clone(),
        Some(cfg.model.clone()),
        Some(cfg.base_url.clone()),
        None,
        None,
        None,
        None,
        attachments_json,
    )?;

    let base_prompt = conversation
        .system_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());
    let model_label = crate::pricing::cached_model_name(db, &cfg.base_url, &cfg.model)
        .unwrap_or_else(|| cfg.model.clone());
    let system_prompt = build_system_prompt(&base_prompt, &model_label, &cfg.preferences, memory);
    let mut messages = build_messages(&system_prompt, &history, &content, &attachments)?;
    let tools_list = tools::tool_specs(tools::brave_script_available());

    let mut usage_total = json!({});
    let mut rounds: u32 = 0;
    let stop_reason: String;
    let mut final_text = String::new();
    let mut final_thinking: Option<String> = None;
    // Reasoning produced by the last tool round, echoed on the assistant
    // tool-call message of this turn's subsequent requests. Never replays
    // previous turns (build_messages stays reasoning-free).
    let mut pending_reasoning: Option<String> = None;

    loop {
        let body = completion_body(
            &cfg.model,
            &messages,
            &tools_list,
            &cfg.thinking_level,
            if cfg.echo_reasoning_content {
                pending_reasoning.as_deref()
            } else {
                None
            },
        )?;
        let response = match open_completion_stream(&cfg.base_url, api_key, &body).await {
            Ok(r) => r,
            Err(e) => {
                stop_reason = "error".into();
                sink.emit(StreamEvent::Error { message: e });
                break;
            }
        };

        let acc = run_completion(response, sink, &flag).await;
        sum_usage(&mut usage_total, &acc.usage);
        track_context(&mut usage_total, &acc.usage);

        let aborted = flag.load(Ordering::SeqCst);
        if aborted || acc.failed || rounds >= tools::MAX_TOOL_ROUNDS {
            if aborted {
                stop_reason = "aborted".into();
            } else if acc.failed {
                stop_reason = "error".into();
            } else {
                stop_reason = "length".into(); // tool-round cap reached
            }
            final_text = acc.text;
            final_thinking = (!acc.thinking.is_empty()).then_some(acc.thinking);
            break;
        }

        if acc.finish.as_deref() == Some("tool_calls") && !acc.tool_calls.is_empty() {
            rounds += 1;
            let thinking = (!acc.thinking.is_empty()).then_some(acc.thinking.clone());
            pending_reasoning = thinking.clone();
            // Attach the round's reasoning to the first assistant row we store:
            // the preamble text if the model wrote any (ignore whitespace-only
            // preambles), otherwise the tool-call row.
            let has_preamble = !acc.text.trim().is_empty();
            if has_preamble {
                db::insert_message(
                    db,
                    conversation_id.clone(),
                    "assistant".into(),
                    acc.text,
                    Some(cfg.model.clone()),
                    Some(cfg.base_url.clone()),
                    None,
                    thinking.clone(),
                    None,
                    None,
                )?;
            }
            let tool_row_thinking = if has_preamble { None } else { thinking };
            db::insert_message(
                db,
                conversation_id.clone(),
                "assistant".into(),
                encode_tool_calls(&acc.tool_calls),
                Some(cfg.model.clone()),
                Some(cfg.base_url.clone()),
                None,
                tool_row_thinking,
                None,
                None,
            )?;
            messages.push(
                ChatCompletionRequestAssistantMessageArgs::default()
                    .tool_calls::<Vec<_>>(
                        acc.tool_calls
                            .iter()
                            .map(|c| {
                                async_openai::types::chat::ChatCompletionMessageToolCalls::Function(
                                    async_openai::types::chat::ChatCompletionMessageToolCall {
                                        id: c.id.clone(),
                                        function: async_openai::types::chat::FunctionCall {
                                            name: c.name.clone(),
                                            arguments: c.arguments.to_string(),
                                        },
                                    },
                                )
                            })
                            .collect(),
                    )
                    .build()
                    .map_err(|e| e.to_string())?
                    .into(),
            );

            let mut aborted_mid_tools = false;
            for call in &acc.tool_calls {
                sink.emit(StreamEvent::ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.to_string(),
                    gated: tools::is_gated(&call.name),
                });

                let mut approved = !tools::is_gated(&call.name);
                if !approved {
                    let mut rx = approvals.register(call.id.clone());
                    loop {
                        tokio::select! {
                            verdict = &mut rx => {
                                approved = verdict.unwrap_or(false);
                                break;
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                                if flag.load(Ordering::SeqCst) {
                                    break;
                                }
                            }
                        }
                    }
                }

                let output = if approved {
                    if call.name == "web_search" {
                        let query = call
                            .arguments
                            .get("query")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let count = call.arguments.get("count").and_then(Value::as_u64);
                        mcp.web_search(query, count).await
                    } else if matches!(call.name.as_str(), "save_memory" | "read_memory") {
                        crate::memory::execute_tool(call, &conversation_id, memory).await
                    } else {
                        tools::execute_host_tool(call, shell, &conversation_id).await
                    }
                } else {
                    ToolOutput {
                        content: "The user denied this tool call.".into(),
                        is_error: true,
                    }
                };

                sink.emit(StreamEvent::ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    ok: !output.is_error,
                    output: output.content.clone(),
                });

                db::insert_message(
                    db,
                    conversation_id.clone(),
                    "tool".into(),
                    encode_tool_result(&call.id, &call.name, &output),
                    Some(cfg.model.clone()),
                    Some(cfg.base_url.clone()),
                    None,
                    None,
                    None,
                    None,
                )?;
                messages.push(
                    ChatCompletionRequestToolMessageArgs::default()
                        .content(output.content.clone())
                        .tool_call_id(call.id.clone())
                        .build()
                        .map_err(|e| e.to_string())?
                        .into(),
                );

                if flag.load(Ordering::SeqCst) {
                    aborted_mid_tools = true;
                    break;
                }
            }

            if aborted_mid_tools {
                stop_reason = "aborted".into();
                final_thinking = None; // reasoning was attached to the round's rows
                final_text = String::new();
                break;
            }
            continue;
        }

        // Final answer round.
        final_text = acc.text;
        final_thinking = (!acc.thinking.is_empty()).then_some(acc.thinking);
        stop_reason = acc.finish.unwrap_or_else(|| "stop".to_string());
        break;
    }

    approvals.deny_all();

    // Freeze the turn's price at the model/rates in effect now, so switching
    // models later never retroactively re-prices turns already billed.
    if usage_total.get("prompt_tokens").is_some() || usage_total.get("completion_tokens").is_some()
    {
        let pricing = crate::pricing::resolve_for(db, cfg, &cfg.model);
        if let Some(cost) = crate::pricing::cost_of_usage(&usage_total, &pricing) {
            usage_total["cost"] = json!(cost);
        }
    }

    db::insert_message(
        db,
        conversation_id.clone(),
        "assistant".into(),
        final_text.clone(),
        Some(cfg.model.clone()),
        Some(cfg.base_url.clone()),
        None,
        final_thinking,
        Some(usage_total.to_string()),
        Some(stop_reason.clone()),
    )?;

    sink.emit(StreamEvent::Done { stop_reason });
    Ok(())
}

/// Stream an assistant reply for the given conversation: read config + key,
/// register the cancel flag, and hand off to the tool-loop core.
#[tauri::command]
pub async fn stream_chat(
    app: tauri::AppHandle,
    conversation_id: String,
    content: String,
    attachments: Option<Vec<crate::attachments::Attachment>>,
    channel: Channel<StreamEvent>,
) -> Result<(), String> {
    let config = app.state::<ConfigState>().get();
    let api_key = crate::secrets::get()?
        .ok_or_else(|| "API key not set — open Settings and add your key.".to_string())?;
    if config.model.trim().is_empty() {
        return Err("No model selected — set a model in Settings.".into());
    }

    let db = app.state::<db::Db>();
    let approvals = app.state::<tools::ApprovalRegistry>();
    let mcp = app.state::<tools::McpClient>();
    let shell = app.state::<ShellRegistry>();
    let memory = app.state::<crate::memory::MemoryState>();

    let flag = app
        .state::<StreamRegistry>()
        .register(conversation_id.clone());
    let sink = ChannelSink(&channel);

    let result = run_chat_turn(
        &db,
        &config,
        &api_key,
        &mcp,
        &shell,
        &memory,
        &approvals,
        flag,
        &sink,
        conversation_id.clone(),
        content,
        attachments.unwrap_or_default(),
    )
    .await;

    app.state::<StreamRegistry>().remove(&conversation_id);
    approvals.deny_all();
    result
}

/// Abort a running stream for a conversation. The streaming loop notices the
/// flag between chunks, preserves partial text, and marks `stop_reason = aborted`.
#[tauri::command]
pub fn stop_chat(app: tauri::AppHandle, conversation_id: String) -> Result<(), String> {
    app.state::<StreamRegistry>().cancel(&conversation_id);
    app.state::<tools::ApprovalRegistry>().deny_all();
    Ok(())
}

#[cfg(test)]
mod tests;
