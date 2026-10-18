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
use tokio::sync::Notify;
use tokio_stream::StreamExt;

use crate::config::{AppConfig, ConfigState};
use crate::db;
use crate::shell::ShellExecutor;
use crate::tools::{self, ToolCall, ToolOutput};

pub(crate) const DEFAULT_SYSTEM_PROMPT: &str =
    r#"You are a helpful general-purpose assistant. Be concise, accurate, and clear."#;

/// Fraction of the model's context window at which a turn forces a compaction
/// before it runs. The window is measured against the exact request we are about
/// to send (see [`estimate_context_tokens`]).
pub(crate) const COMPACTION_THRESHOLD: f64 = 0.8;
/// How many tokens of the most recent history are kept verbatim after a
/// compaction; everything older is folded into the summary.
pub(crate) const COMPACTION_KEEP_TOKENS: u64 = 15_000;
/// Rough chars-per-token, matching the frontend meter and the backfill sizing.
/// Only used to pick the verbatim/compacted split and to trigger compaction.
const CHARS_PER_TOKEN: u64 = 4;
/// Fixed token cost attributed to each image part by [`estimate_context_tokens`],
/// in place of the (meaningless) length of its base64 payload.
const IMAGE_TOKENS: u64 = 1_200;

/// Cancellation handle for one streaming turn.
///
/// A plain `AtomicBool` was only ever *polled*, so a Stop could appear dead for
/// seconds: the request open and a running tool call (`bash` can take 60 s, a
/// `web_fetch` 30 s) never looked at it at all. This pairs the flag — which
/// stays the cheap check on hot paths — with a [`Notify`], so callers can
/// `select!` on [`CancelToken::cancelled`] and drop an in-flight future at once
/// (dropping the tool future kills its child process via `kill_on_drop`).
///
/// Cheap to clone; every clone observes the same cancellation.
#[derive(Clone, Default, Debug)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Whether two handles refer to the same turn's cancellation state. Clones
    /// share it; two tokens from separate [`CancelToken::new`] calls never do.
    /// [`StreamRegistry`] uses this to remove only its own registration.
    fn same_turn(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }

    /// Request cancellation. Idempotent: later calls are no-ops, and the flag is
    /// set *before* the wakeup so a waiter that starts during the call observes
    /// it too.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // `notify_waiters` only wakes futures that are already registered, which
        // is why `cancelled` re-checks the flag after registering.
        self.notify.notify_waiters();
    }

    /// Resolve as soon as the token is cancelled, immediately when it already
    /// is. Race-free: interest is registered with `enable()` *before* the flag is
    /// re-checked, so a `cancel()` landing in the window between the first check
    /// and registration cannot be missed.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// Registration of a conversation's in-flight stream, so the UI can abort it and
/// the reflection scheduler can stay out of a conversation that is mid-turn.
#[derive(Default)]
pub struct StreamRegistry {
    streams: Mutex<HashMap<String, CancelToken>>,
}

impl StreamRegistry {
    /// Register a conversation as streaming and hand back its cancel token. The
    /// token stays valid after [`remove`](Self::remove) — cancellation is always
    /// observable to whoever holds it.
    pub fn register(&self, id: String) -> CancelToken {
        let token = CancelToken::new();
        self.streams.lock().unwrap().insert(id, token.clone());
        token
    }

    /// Abort the conversation's in-flight stream *now*: in-flight work blocked on
    /// the request open or a tool call is dropped immediately, not on the next
    /// poll.
    pub fn cancel(&self, id: &str) {
        if let Some(token) = self.streams.lock().unwrap().get(id) {
            token.cancel();
        }
    }

    /// Drop the conversation's registration, but **only while it still belongs
    /// to `token`**.
    ///
    /// A turn's tail always runs `remove`, and Stop is immediate: the UI unblocks
    /// and can resend before the old turn's tail executes, registering a *new*
    /// token for the same conversation. An unconditional remove would then delete
    /// the new registration — Stop on the new turn would silently no-op and
    /// `is_streaming` would report `false` while it is still live (which also
    /// defeats the reflection scheduler's interlock). Removing only our own
    /// token leaves a subsequent turn's registration untouched.
    pub fn remove(&self, id: &str, token: &CancelToken) {
        let mut streams = self.streams.lock().unwrap();
        if streams.get(id).is_some_and(|current| current.same_turn(token)) {
            streams.remove(id);
        }
    }

    /// Whether a chat turn is currently streaming for this conversation. Used by
    /// the memory-reflection scheduler to avoid running mid-turn.
    pub fn is_streaming(&self, id: &str) -> bool {
        self.streams.lock().unwrap().contains_key(id)
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
    /// The provider's failure text, kept so a bulk caller can tell a transient
    /// rate limit from a permanent rejection.
    error: Option<String>,
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

/// Extract reasoning text from a content-array part. Different providers put the
/// reasoning in different fields: `text` (some OpenAI-compatible), `thinking`
/// (Claude/thinking-style), or `content` (Responses-style reasoning parts, which
/// is a string or an array of strings). Returns `None` when the part carries no
/// readable reasoning, so it's skipped rather than emitted as an empty trace.
fn reasoning_part_text(part: &Value) -> Option<String> {
    for key in ["text", "thinking", "content"] {
        if let Some(s) = part.get(key).and_then(Value::as_str) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    if let Some(arr) = part.get("content").and_then(Value::as_array) {
        let joined: String = arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("");
        if !joined.trim().is_empty() {
            return Some(joined);
        }
    }
    None
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
        let message = error_message(err);
        out.failed = true;
        out.error = Some(message.clone());
        sink.emit(StreamEvent::Error { message });
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
            out.error = Some("provider error".to_string());
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
                        if let Some(t) = reasoning_part_text(item) {
                            out.thinking.push_str(&t);
                            sink.emit(StreamEvent::ThinkingDelta { text: t });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    // DeepSeek/Qwen-style reasoning, streamed in its own delta field. Some
    // providers alias it as `thinking` or `reasoning` (a plain string) rather
    // than `reasoning_content`; accept any of those, deepest-match, but only
    // once (so a provider that sends multiple aliases isn't double-counted).
    for key in ["reasoning_content", "thinking", "reasoning"] {
        if let Some(r) = delta.get(key).and_then(Value::as_str) {
            if !r.is_empty() {
                out.thinking.push_str(r);
                sink.emit(StreamEvent::ThinkingDelta { text: r.to_string() });
                break;
            }
        }
    }
    // Streamed tool-call fragments, accumulated by index.
    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
            let slot = calls.entry(index).or_insert((None, None, String::new()));
            // Some gateways (e.g. InferX) repeat the whole `tool_calls` array on
            // every fragment chunk with `id`/`function.name` blanked to empty
            // strings, only carrying new text in `arguments`. Adopt metadata only
            // when non-empty (first delta wins); always append argument text.
            if let Some(id) = tc.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    slot.0 = Some(id.to_string());
                }
            }
            if let Some(f) = tc.get("function") {
                if let Some(name) = f.get("name").and_then(Value::as_str) {
                    if !name.is_empty() {
                        slot.1 = Some(name.to_string());
                    }
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
    });
    // Omit the field entirely when there are no tools: some providers reject an
    // empty array, and the extraction pass deliberately advertises none.
    if !tools.is_empty() {
        body["tools"] = serde_json::to_value(tools).map_err(|e| e.to_string())?;
    }
    let level = thinking_level.trim();
    if !level.is_empty() && level != "default" {
        body["reasoning_effort"] = json!(level);
    }
    Ok(body)
}

/// Consume a completion SSE stream, emitting `Delta`/`ThinkingDelta`/`Error`
/// events as chunks arrive, accumulating streamed tool-call fragments, and
/// stopping early when the token is cancelled (abort). The stream is read in its
/// raw form (not through async-openai's typed parser) so provider reasoning
/// fields like `reasoning_content` are not dropped.
async fn run_completion(
    response: reqwest::Response,
    sink: &dyn EventSink,
    token: &CancelToken,
) -> RoundAccum {
    let mut out = RoundAccum::default();
    let mut calls: HashMap<u32, (Option<String>, Option<String>, String)> = HashMap::new();
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut closed = false;

    'consume: loop {
        if token.is_cancelled() {
            break;
        }
        while let Some(event) = take_first_event(&mut buf) {
            if token.is_cancelled() {
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
            // Wait on cancellation rather than re-checking on a timer: a stalled
            // stream (or one that outpaces a poll interval) aborts immediately.
            biased;
            _ = token.cancelled() => break,
            maybe = stream.next() => {
                match maybe {
                    None => closed = true,
                    Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
                    Some(Err(e)) => {
                        let message = format!("stream error: {e}");
                        out.failed = true;
                        out.error = Some(message.clone());
                        sink.emit(StreamEvent::Error { message });
                        break;
                    }
                }
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
pub(crate) fn user_request_message(
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

/// Build the synthetic user message that carries an image loaded by `read_file`
/// on a multimodal model. Reuses [`user_request_message`]'s image-part
/// construction (async-openai's tool-message content has no image variant).
fn image_request_message(
    call: &ToolCall,
    data_url: String,
) -> Result<ChatCompletionRequestMessage, String> {
    let name = call
        .arguments
        .get("path")
        .and_then(Value::as_str)
        .and_then(|p| {
            std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "image".to_string());
    let attachment = crate::attachments::Attachment {
        id: format!("read_file-image-{}", call.id),
        name,
        mime: String::new(),
        size: 0,
        kind: "image".into(),
        data_url: Some(data_url),
        text: None,
    };
    user_request_message("", &[attachment])
}

/// Parse the stored attachment JSON on a user row (empty when absent/invalid).
fn row_attachments(row: &db::MessageRow) -> Vec<crate::attachments::Attachment> {
    row.attachments
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// Convert stored message rows into request messages (no system prompt, no
/// trailing turn). Assistant rows holding a tool-call payload and tool rows
/// holding results are converted back into their request shapes so multi-round
/// history works. `memory` notes are bookkeeping and are skipped.
pub(crate) fn build_history_messages(
    history: &[db::MessageRow],
) -> Result<Vec<ChatCompletionRequestMessage>, String> {
    let mut out = Vec::new();
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
            // Consolidation notes and clock notes are bookkeeping, but both surface
            // to the model as lightweight system messages: `memory` so it knows
            // memory was updated (and doesn't re-save), `clock` so the current time
            // and the device's UTC offset are in context on every turn. Live and
            // reflection build history through here, so the shared prefix stays
            // identical.
            "memory" | "clock" => {
                out.push(
                    ChatCompletionRequestSystemMessageArgs::default()
                        .content(row.content.clone())
                        .build()
                        .map_err(|e| e.to_string())?
                        .into(),
                );
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Convert a system prompt, an optional compaction summary, existing history and
/// a fresh user turn into request messages.
///
/// Layout is exactly: system prompt → compaction summary (as a system message)
/// → verbatim history → the new user turn. The leading part is shared with the
/// reflection pass through [`build_context`].
///
/// Since the live turn now interleaves a transient time note, `run_chat_turn`
/// builds its request inline; this helper remains as the canonical shape the
/// tests assert against.
#[cfg(test)]
fn build_messages(
    system_prompt: &str,
    compaction_summary: Option<&str>,
    history: &[db::MessageRow],
    user_content: &str,
    user_attachments: &[crate::attachments::Attachment],
) -> Result<Vec<ChatCompletionRequestMessage>, String> {
    let mut out = build_context(system_prompt, compaction_summary, history)?;
    out.push(user_request_message(user_content, user_attachments)?);
    Ok(out)
}

/// The leading context shared by a live turn and a reflection pass: the system
/// prompt, the compaction summary when one exists, then the verbatim history.
/// Both paths build this exact prefix so it stays prompt-cacheable.
pub(crate) fn build_context(
    system_prompt: &str,
    compaction_summary: Option<&str>,
    history: &[db::MessageRow],
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
    if let Some(summary) = compaction_summary.filter(|s| !s.trim().is_empty()) {
        out.push(
            ChatCompletionRequestSystemMessageArgs::default()
                .content(summary.to_string())
                .build()
                .map_err(|e| e.to_string())?
                .into(),
        );
    }
    out.extend(build_history_messages(history)?);
    Ok(out)
}

/// Split stored history at the compaction watermark: the summary of everything
/// older, and the index of the first message to send verbatim.
///
/// The boundary is matched by message **id** (`compaction_through`), never by
/// index — cross-device pull renumbers `index`. A boundary that is missing (for
/// example a summary synced from a device that had compaction onto one that does
/// not, since the boundary itself is local-only) makes the summary be ignored,
/// rather than guessing a split that could drop context.
pub(crate) fn compacted_history(
    conversation: &db::Conversation,
    history: &[db::MessageRow],
) -> (Option<String>, usize) {
    // BOTH halves are required: the boundary locates the split, the summary is
    // what replaces the folded messages. If either is missing, ignore the
    // compaction and send the full history. This matters because they have
    // different lifetimes — a sync pull applies last-writer-wins to
    // `compaction_summary` (it is on the synced row) but never to the local-only
    // `compaction_through`, so a summary can be nulled while the boundary
    // survives. Slicing then would drop context with nothing injected in its
    // place.
    let (Some(boundary), Some(summary)) = (
        conversation.compaction_through.as_deref(),
        conversation.compaction_summary.as_deref(),
    ) else {
        return (None, 0);
    };
    if summary.trim().is_empty() {
        return (None, 0);
    }
    match history.iter().position(|m| m.id == boundary) {
        Some(pos) => (Some(summary.to_string()), pos + 1),
        None => (None, 0),
    }
}

/// Index of the first history message to keep verbatim, so roughly the last
/// `keep_tokens` tokens survive and everything before is folded. The kept tail
/// must never begin with a `tool` row: its assistant tool-call would have been
/// folded away, orphaning the result (an invalid request), so the whole trailing
/// tool group is folded instead.
fn keep_from(history: &[db::MessageRow], keep_tokens: u64) -> usize {
    let mut acc = 0u64;
    let mut start = 0usize;
    for (i, m) in history.iter().enumerate().rev() {
        // ~4 chars/token plus a small per-message wire overhead.
        acc += m.content.len() as u64 / CHARS_PER_TOKEN + 8;
        if acc >= keep_tokens {
            start = i;
            break;
        }
    }
    // A kept tail must never begin on a `tool` row: its assistant tool-call
    // would have been folded away, orphaning the result (an invalid request), so
    // the whole trailing tool group is folded instead. Only adjust when a tail is
    // actually being kept (`start > 0`); a leading tool row means nothing was
    // folded, so leave `start` at 0 and skip compaction.
    while start > 0 && start < history.len() && history[start].role == "tool" {
        start += 1;
    }
    start
}

/// Rough token size of a request we are about to send (~4 chars/token over the
/// serialized messages). Only used to decide whether to compact.
///
/// Data-URL payloads (`data:<mime>;base64,…`) are excluded from the character
/// count and replaced with a fixed per-image cost. A data URL's length tracks
/// the *encoded file size*, not the tokens a provider charges for an image, and
/// history replays every image on every turn (`build_history_messages` →
/// `user_request_message`) — so counting those bytes as text is what made
/// compaction fire early on any conversation holding a pasted screenshot.
fn estimate_context_tokens(messages: &[ChatCompletionRequestMessage]) -> u64 {
    let Ok(json) = serde_json::to_string(messages) else {
        return 0;
    };
    let mut text_len = json.len();
    let mut images = 0u64;
    let mut rest = json.as_str();
    while let Some(pos) = rest.find(";base64,") {
        let after = &rest[pos + ";base64,".len()..];
        match after.find('"') {
            Some(end) => {
                text_len -= after[..end].len();
                images += 1;
                rest = &after[end..];
            }
            None => break,
        }
    }
    text_len as u64 / CHARS_PER_TOKEN + images * IMAGE_TOKENS
}

/// Estimated tokens contributed by the not-yet-sent user turn: its text plus
/// each attachment (images at a fixed cost, other files by decoded size). Added
/// to the provider's concrete count for everything already sent, so the whole
/// history is measured the way the provider actually bills it rather than by a
/// fixed per-image constant applied to every replayed image.
fn estimate_turn_tokens(content: &str, attachments: &[crate::attachments::Attachment]) -> u64 {
    let mut tokens = content.len() as u64 / CHARS_PER_TOKEN + 8;
    for a in attachments {
        if a.kind == "image" {
            tokens += IMAGE_TOKENS;
        } else {
            tokens += a.size / CHARS_PER_TOKEN;
        }
    }
    tokens
}

/// The context size the provider last reported for this conversation (the newest
/// assistant turn's `context_tokens`, else its prompt+completion). Mirrors the
/// frontend meter, and is more accurate than the char estimate when present.
fn reported_context_tokens(history: &[db::MessageRow]) -> Option<u64> {
    for row in history.iter().rev() {
        if row.role != "assistant" {
            continue;
        }
        let Some(usage) = row.usage.as_deref() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(usage) else {
            continue;
        };
        if let Some(n) = v.get("context_tokens").and_then(Value::as_u64) {
            return Some(n);
        }
        let p = v.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
        let c = v.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0);
        if p > 0 || c > 0 {
            return Some(p + c);
        }
    }
    None
}

/// Sink for internal passes (compaction) that have no UI: events are discarded.
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _ev: StreamEvent) {}
}

const COMPACTION_SYSTEM: &str = "You are a conversation summarizer. Produce one dense, factual \
summary of the conversation so far: decisions, facts the user stated, open threads, and any \
state needed to continue. Preserve concrete details (names, numbers, file paths, identifiers). \
No commentary, preamble, or headings — output only the summary.";

const COMPACTION_INSTRUCTION: &str = "Fold the exchange above into the running summary. Keep it \
self-contained and dense; drop pleasantries and resolved trivia. Output only the updated summary.";

/// One summarization round for a compaction pass: no tools, single completion.
/// Returns the summary text and the round's usage (for the maintenance cost row).
async fn summarize_history(
    cfg: &AppConfig,
    api_key: &str,
    previous: Option<&str>,
    fold: &[db::MessageRow],
    token: &CancelToken,
) -> Result<(String, Value), String> {
    let system = match previous.filter(|p| !p.trim().is_empty()) {
        Some(prev) => format!("{COMPACTION_SYSTEM}\n\nSummary so far:\n{prev}"),
        None => COMPACTION_SYSTEM.to_string(),
    };
    let mut messages = vec![
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system)
            .build()
            .map_err(|e| e.to_string())?
            .into(),
    ];
    // Clock notes are per-turn bookkeeping: folding them into the summary would
    // spend tokens to record that the model was told the time, repeatedly.
    let fold: Vec<db::MessageRow> = fold
        .iter()
        .filter(|m| m.role != "clock")
        .cloned()
        .collect();
    messages.extend(build_history_messages(&fold)?);
    messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(COMPACTION_INSTRUCTION.to_string())
            .build()
            .map_err(|e| e.to_string())?
            .into(),
    );

    let body = completion_body(&cfg.model, &messages, &[], &cfg.thinking_level, None)?;
    let response = tokio::select! {
        biased;
        _ = token.cancelled() => return Ok((String::new(), json!({}))),
        opened = open_completion_stream(&cfg.base_url, api_key, &body) => opened?,
    };
    let acc = run_completion(response, &NullSink, token).await;
    if acc.failed {
        return Err(acc.error.unwrap_or_else(|| "compaction failed".into()));
    }
    let usage = acc.usage.unwrap_or_else(|| json!({}));
    // An aborted pass must not persist a partial summary; returning no text makes
    // the caller skip the compaction and let the turn abort normally.
    if token.is_cancelled() {
        return Ok((String::new(), usage));
    }
    Ok((acc.text, usage))
}

/// Fold newly-old history into the conversation's summary and persist it.
/// Returns the new summary and the verbatim tail start, or `None` when there is
/// nothing new to fold. The pass is idempotent and resumable: the previous
/// summary plus only the messages after the old watermark are summarized, so a
/// long conversation is never re-summarized from scratch.
///
/// Runs inside the live turn, so the conversation is registered as streaming and
/// the reflection scheduler/its watcher stay out (see [`db::set_compaction`]).
async fn compact_now(
    db: &db::Db,
    conversation: &db::Conversation,
    history: &[db::MessageRow],
    cfg: &AppConfig,
    use_bundled_context: bool,
    api_key: &str,
    token: &CancelToken,
) -> Result<Option<(String, usize)>, String> {
    let start = keep_from(history, COMPACTION_KEEP_TOKENS);
    if start == 0 {
        return Ok(None); // everything is still within the verbatim budget
    }
    // Where the previous summary ended, so we only fold the newly-old stretch.
    let prev_start = match conversation.compaction_through.as_deref() {
        Some(id) => history
            .iter()
            .position(|m| m.id == id)
            .map(|p| p + 1)
            .unwrap_or(0),
        None => 0,
    };
    if start <= prev_start {
        return Ok(None);
    }

    let through_id = history[start - 1].id.clone();
    let (summary, usage) = match summarize_history(
        cfg,
        api_key,
        conversation.compaction_summary.as_deref(),
        &history[prev_start..start],
        token,
    )
    .await
    {
        Ok(x) => x,
        // Best-effort: a summarizer failure must never break the user's turn; it
        // just runs uncompacted (and may hit the provider limit, as before).
        Err(e) => {
            eprintln!("context compaction: {e}");
            return Ok(None);
        }
    };
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Ok(None); // aborted, or nothing produced
    }

    // Best-effort persistence: if the write fails, skip compaction for this turn
    // and run uncompacted rather than failing the user's turn.
    if let Err(e) = db::set_compaction(db, &conversation.id, &summary, &through_id) {
        eprintln!("context compaction: persisting summary failed: {e}");
        return Ok(None);
    }

    // Record the spend as background maintenance (kept out of chat cost), the
    // same as memory reflection.
    let pricing = crate::pricing::resolve_for_base(
        db,
        &cfg.base_url,
        &cfg.model_overrides,
        &cfg.model,
        use_bundled_context,
    );
    let cost = crate::pricing::cost_of_usage(&usage, &pricing);
    let _ = db::insert_reflection(
        db,
        &conversation.id,
        &cfg.model,
        Some(&usage.to_string()),
        cost,
        &[],
        "Context compacted (older messages summarized)",
    );

    Ok(Some((summary, start)))
}

/// Format an epoch millisecond timestamp as `YYYY-MM-DD` in UTC. Hand-rolled
/// civil-from-days logic copied from `import.rs` / `reflection::format_day`
/// (chrono is used only for the device's local UTC offset).
pub(crate) fn format_date(ms: i64) -> String {
    let days = (ms / 1000).div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Format an epoch millisecond timestamp as `YYYY-MM-DD HH:MM` in UTC for the
/// clock note.
pub(crate) fn format_date_time(ms: i64) -> String {
    let days = (ms / 1000).div_euclid(86_400);
    let secs = (ms / 1000).rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hour = secs / 3_600;
    let minute = (secs % 3_600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

/// UTC weekday name for an epoch millisecond timestamp (1970-01-01, day 0, was
/// a Thursday). Hand-rolled to pair with `format_date`.
pub(crate) fn weekday_name(ms: i64) -> &'static str {
    const NAMES: [&str; 7] = [
        "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
    ];
    let days = (ms / 1000).div_euclid(86_400);
    // 1970-01-01 is a Thursday, i.e. index 4 in `NAMES`.
    NAMES[(days + 4).rem_euclid(7) as usize]
}

/// The device's current UTC offset in minutes east of UTC, read from the OS
/// local timezone (DST-aware for the present instant).
pub(crate) fn local_utc_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// Render a minute offset as `+HH:MM` / `-HH:MM` (also handles half-hour zones).
pub(crate) fn format_utc_offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// Format an epoch millisecond timestamp in the device's local timezone for the
/// clock note's local half. The offset is applied by hand so this stays
/// consistent with the UTC formatter above.
pub(crate) fn format_local_date_time(ms: i64, offset_minutes: i32) -> String {
    format_date_time(ms + offset_minutes as i64 * 60_000)
}

/// The clock note's text for this turn: the UTC time + weekday, the device's
/// local wall time, and the local UTC offset. Split out from
/// [`clock_note_message`] because the same string is both persisted (as a
/// `clock` row) and injected into the request.
///
/// The offset is read fresh from the OS every turn, so a DST change or a move to
/// another timezone shows up immediately, and the local time is spelled out so
/// the model never has to do the arithmetic (or get its sign wrong).
pub(crate) fn clock_note_text(now_ms: i64) -> String {
    let offset = local_utc_offset_minutes();
    format!(
        "Current time: {} UTC ({}). The user's local time is {} (UTC{}).",
        format_date_time(now_ms),
        weekday_name(now_ms),
        format_local_date_time(now_ms, offset),
        format_utc_offset(offset),
    )
}

/// Wrap the clock note in the system message injected before the new user turn.
///
/// The note is **persisted** as a `clock` row (hidden from the timeline) rather
/// than only injected, because of prompt caching: a transient note sits at the
/// end of one request but not the next, so the sequence shifts at its insertion
/// point and every turn has to re-send the previous exchange — uncached — from
/// there on. A stored row makes each request an exact extension of the last one,
/// so the provider's prefix cache covers the whole conversation. It costs ~25
/// tokens (~100 chars) per turn, which is far cheaper than re-billing one exchange
/// per turn.
pub(crate) fn clock_note_message(text: &str) -> Result<ChatCompletionRequestMessage, String> {
    Ok(ChatCompletionRequestSystemMessageArgs::default()
        .content(text.to_string())
        .build()
        .map_err(|e| e.to_string())?
        .into())
}

/// Base system prompt, with explicit user preferences injected at the top and
/// the active model named, then the bounded memory block appended. The
/// preferences field is user-authored in Settings, so it is labelled as
/// authoritative — distinct from anything inferred into memory. Memory bodies
/// for non-core files stay out of context until `read_memory` is called.
pub(crate) fn build_system_prompt(
    base: &str,
    model_label: &str,
    preferences: &str,
    memory: &crate::memory::MemoryState,
    conversation_started: Option<i64>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !preferences.trim().is_empty() {
        parts.push(format!(
            "Explicit user preferences (set directly by the user; authoritative — these take \
             precedence over anything inferred in long-term memory):\n{}",
            preferences.trim()
        ));
    }
    parts.push(base.trim().to_string());
    if !model_label.trim().is_empty() {
        parts.push(format!(
            "You are currently running as the model \"{}\".",
            model_label.trim()
        ));
    }
    // Host tools run on the user's own device, not on a server.
    parts.push(
        "Host tools (bash, read_file, write_file) execute on the user's own device — the machine \
         they are sending from — not on a remote server."
            .to_string(),
    );
    if let Some(ms) = conversation_started {
        parts.push(format!(
            "This conversation started on {} ({}) UTC.",
            format_date(ms),
            weekday_name(ms)
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

/// The system prompt shared by a live turn and a reflection pass: the
/// conversation's own base prompt (falling back to the default), the resolved
/// model label, the explicit preferences, the memory block and the conversation
/// start date. One function, so the two paths cannot drift apart.
pub(crate) fn conversation_system_prompt(
    db: &db::Db,
    cfg: &AppConfig,
    conversation: &db::Conversation,
    base_url: &str,
    model: &str,
    memory: &crate::memory::MemoryState,
) -> String {
    let base_prompt = conversation
        .system_prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());
    let model_label = crate::pricing::cached_model_name(db, base_url, model)
        .unwrap_or_else(|| model.to_string());
    build_system_prompt(
        &base_prompt,
        &model_label,
        &cfg.preferences,
        memory,
        Some(conversation.created_at),
    )
}

/// The tool list every turn advertises. Live, reflection and extraction share it
/// byte-for-byte (prompt-cache alignment), so it is built in one place.
pub(crate) fn chat_tools() -> Vec<async_openai::types::chat::ChatCompletionTools> {
    tools::tool_specs(tools::brave_available())
}

/// Everything a completed tool loop produced.
pub(crate) struct ToolLoopResult {
    pub usage: Value,
    pub text: String,
    pub thinking: Option<String>,
    pub stop_reason: String,
    /// Set when `stop_reason` is `"error"`: the provider's failure text.
    pub error: Option<String>,
    /// Images fetched during the turn (`fetch_image`), to be stored on the
    /// assistant row. Populated on the abort/error paths too, so a partial turn
    /// keeps the images it already attached.
    pub attachments: Vec<crate::attachments::Attachment>,
}

/// A dispatched tool outcome, plus an optional image the model should see and an
/// optional attachment to persist on the assistant's reply.
///
/// async-openai 0.41 tool messages are text-only (`ChatCompletionRequestToolMessageContentPart`
/// has only `Text`), so a `read_file` call that loads an image cannot ride the
/// tool result. Instead [`dispatch_tool`] returns the image here and the tool
/// loop injects it as a synthetic user `image_url` part after the tool result.
struct DispatchedTool {
    output: ToolOutput,
    /// `data:<mime>;base64,…` for an image loaded by `read_file` (or fetched by
    /// `fetch_image`) on a multimodal model; `None` for every other call.
    image: Option<String>,
    /// An image downloaded by `fetch_image`, persisted in the assistant row's
    /// attachment list so it shows in the transcript and survives a reload.
    attachment: Option<crate::attachments::Attachment>,
}

impl DispatchedTool {
    fn plain(output: ToolOutput) -> Self {
        Self {
            output,
            image: None,
            attachment: None,
        }
    }
}

/// The result recorded for a tool call the user stopped before it produced
/// output.
///
/// Stop still writes a result row: an assistant `tool_calls` row must be followed
/// by exactly one result per call, or the next turn replays an invalid request.
/// The wording is distinct from a real Deny ("The user denied this tool call."),
/// so the transcript says which one happened.
fn aborted_tool_output() -> ToolOutput {
    ToolOutput::err("The user aborted this tool call.".into())
}

/// Handle a `read_file` call for an image path. Multimodal models get a short
/// text result plus the image (returned for injection); text-only models get a
/// result telling them they cannot view it, so they can relay that to the user.
fn read_file_image(path: &str, cfg: &AppConfig, db: Option<&db::Db>) -> DispatchedTool {
    // Oversized images are never read (no data URL, no injection): base64
    // inflation plus re-sending on every remaining round of the turn makes a
    // huge image expensive, and most providers cap the request body anyway.
    // Hand the model a plain text result so it can relay the limitation.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() && meta.len() > crate::attachments::MAX_IMAGE_BYTES {
            return DispatchedTool::plain(ToolOutput::ok(format!(
                "read_file: \"{path}\" is an image, but it is too large to view \
                 ({} bytes; the limit is {} bytes). Tell the user you cannot see its \
                 contents.",
                meta.len(),
                crate::attachments::MAX_IMAGE_BYTES
            )));
        }
    }
    let (mime, data_url) = match crate::attachments::read_image(path) {
        Ok(x) => x,
        Err(e) => return DispatchedTool::plain(ToolOutput::err(format!("read_file: {e}"))),
    };
    // Vision is a property of the *turn's* active provider/model, resolved the
    // same way as the picker: a user modality override → models.dev → the
    // text-only default. A model we know nothing about (custom provider, no
    // override) stays not-vision, so we never inject an image part a text-only
    // endpoint would reject.
    let vision = db
        .map(|db| crate::pricing::capabilities_for(db, cfg, &cfg.model).vision())
        .unwrap_or(false);
    if vision {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        DispatchedTool {
            output: ToolOutput::ok(format!(
                "read_file: loaded image \"{path}\" ({mime}, {size} bytes). The image is \
                 attached below; describe or analyze it as requested."
            )),
            image: Some(data_url),
            attachment: None,
        }
    } else {
        DispatchedTool::plain(ToolOutput::ok(format!(
            "read_file: \"{path}\" is an image, but the active model (\"{}\") cannot view \
             images. Tell the user you cannot see its contents.",
            cfg.model
        )))
    }
}

/// Wrap a fetched image as a tool outcome. The attachment is always persisted on
/// the assistant's reply (so the transcript keeps it across a reload); the model
/// only *sees* it as an image part when the turn's model is vision-capable,
/// exactly like [`read_file_image`].
fn fetched_image_result(
    attachment: crate::attachments::Attachment,
    cfg: &AppConfig,
    db: Option<&db::Db>,
) -> DispatchedTool {
    let vision = db
        .map(|db| crate::pricing::capabilities_for(db, cfg, &cfg.model).vision())
        .unwrap_or(false);
    let described = format!(
        "fetch_image: attached \"{}\" ({}, {} bytes).",
        attachment.name, attachment.mime, attachment.size
    );
    let output = if vision {
        ToolOutput::ok(format!(
            "{described} The image is attached below; describe or use it as requested."
        ))
    } else {
        ToolOutput::ok(format!(
            "{described} It is attached to your reply for the user. The active model \
             (\"{}\") cannot view images, so you cannot describe its contents.",
            cfg.model
        ))
    };
    DispatchedTool {
        output,
        image: if vision {
            attachment.data_url.clone()
        } else {
            None
        },
        attachment: Some(attachment),
    }
}

/// Execute one model-requested tool. Memory tools are handled in-process, the
/// read-only `web_fetch`/`fetch_image` run without approval, and host tools run
/// with per-call approval (gating is handled by the caller). `read_file`
/// on an image path is intercepted here so the turn's model capability (`cfg`)
/// and the DB metadata cache decide whether the image can be delivered.
async fn dispatch_tool(
    call: &ToolCall,
    brave: &tools::BraveSearch,
    shell: &ShellExecutor,
    memory: &crate::memory::MemoryState,
    cfg: &AppConfig,
    db: Option<&db::Db>,
) -> DispatchedTool {
    if call.name == "read_file" {
        if let Some(path) = call.arguments.get("path").and_then(Value::as_str) {
            if crate::attachments::is_image_path(path) {
                return read_file_image(path, cfg, db);
            }
        }
    }
    if call.name == "fetch_image" {
        return match tools::execute_fetch_image(call).await {
            Ok(attachment) => fetched_image_result(attachment, cfg, db),
            Err(e) => DispatchedTool::plain(ToolOutput::err(e)),
        };
    }
    let output = if call.name.starts_with("brave_") {
        brave.call_tool(&call.name, &call.arguments).await
    } else if matches!(
        call.name.as_str(),
        "save_memory" | "read_memory" | "write_memory" | "memory_str_replace"
    ) {
        crate::memory::execute_tool(call, memory).await
    } else if call.name == "web_fetch" {
        // Read-only and ungated, like the Brave tools, and needs no API key.
        tools::execute_web_tool(call).await
    } else {
        tools::execute_host_tool(call, shell).await
    };
    DispatchedTool::plain(output)
}

/// Which tools a tool loop may actually run. The tool *list* sent to the model is
/// identical in every mode (so the prompt-cache prefix matches); access is
/// enforced at dispatch instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ToolMode {
    /// A live chat turn: all memory tools (`save_memory`, `read_memory`,
    /// `write_memory`, `memory_str_replace`) plus host and web tools.
    Live,
    /// Memory consolidation: all memory tools, no host/web tools.
    Reflection,
    /// Memory extraction (the map half of a backfill): no tools are advertised
    /// at all, because extraction is pure summarization. The gate below is a
    /// guard in case a model emits a tool call anyway.
    Extract,
}

/// The capped tool loop, factored out of [`run_chat_turn`] so the memory
/// reflection pass can reuse it. Streams deltas/tool events to `sink`, echoes a
/// turn's reasoning between tool rounds, and returns the accumulated usage and
/// final text. When `persist` is `Some`, intermediate assistant/tool rows are
/// written to the DB (live chat); when `None`, the loop is purely in memory.
/// `mode` gates which tools may run (see [`ToolMode`]).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_loop(
    cfg: &AppConfig,
    api_key: &str,
    brave: &tools::BraveSearch,
    shell: &ShellExecutor,
    memory: &crate::memory::MemoryState,
    approvals: &tools::ApprovalRegistry,
    source: &str,
    mut messages: Vec<ChatCompletionRequestMessage>,
    tools_list: &[async_openai::types::chat::ChatCompletionTools],
    token: CancelToken,
    sink: &dyn EventSink,
    persist: Option<&db::Db>,
    mode: ToolMode,
) -> Result<ToolLoopResult, String> {
    let mut usage_total = json!({});
    let mut rounds: u32 = 0;
    let stop_reason: String;
    let mut final_text = String::new();
    let mut final_thinking: Option<String> = None;
    let mut final_error: Option<String> = None;
    // Reasoning produced by the last tool round, echoed on the assistant
    // tool-call message of this turn's subsequent requests. Never replays
    // previous turns (build_history_messages stays reasoning-free).
    let mut pending_reasoning: Option<String> = None;
    // Images fetched by `fetch_image` during this turn. They are persisted on the
    // final assistant row, so they are collected before the loop can break on an
    // abort or an error (a partial turn keeps whatever it already produced).
    let mut attachments: Vec<crate::attachments::Attachment> = Vec::new();

    loop {
        let body = completion_body(
            &cfg.model,
            &messages,
            tools_list,
            &cfg.thinking_level,
            if cfg.echo_reasoning_content {
                pending_reasoning.as_deref()
            } else {
                None
            },
        )?;
        let response = tokio::select! {
            // Stop must abort the request open too: a provider that accepted the
            // connection but hasn't answered yet would otherwise ignore the flag
            // until the body started streaming.
            biased;
            _ = token.cancelled() => {
                stop_reason = "aborted".into();
                break;
            }
            opened = open_completion_stream(&cfg.base_url, api_key, &body) => opened,
        };
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                stop_reason = "error".into();
                final_error = Some(e.clone());
                sink.emit(StreamEvent::Error { message: e });
                break;
            }
        };

        let acc = run_completion(response, sink, &token).await;
        sum_usage(&mut usage_total, &acc.usage);
        track_context(&mut usage_total, &acc.usage);

        let aborted = token.is_cancelled();
        if aborted || acc.failed || rounds >= tools::MAX_TOOL_ROUNDS {
            if aborted {
                stop_reason = "aborted".into();
            } else if acc.failed {
                stop_reason = "error".into();
                final_error = acc.error.clone();
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
                if let Some(db) = persist {
                    db::insert_message(
                        db,
                        source.to_string(),
                        "assistant".into(),
                        acc.text,
                        Some(cfg.model.clone()),
                        Some(cfg.base_url.clone()),
                        Some(cfg.thinking_level.clone()),
                        thinking.clone(),
                        None,
                        None,
                    )?;
                }
            }
            let tool_row_thinking = if has_preamble { None } else { thinking };
            if let Some(db) = persist {
                db::insert_message(
                    db,
                    source.to_string(),
                    "assistant".into(),
                    encode_tool_calls(&acc.tool_calls),
                    Some(cfg.model.clone()),
                    Some(cfg.base_url.clone()),
                    Some(cfg.thinking_level.clone()),
                    tool_row_thinking,
                    None,
                    None,
                )?;
            }
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

            // One outcome per requested call; after dispatch, the outcomes become
            // one `tool` result row per call, written as a single transaction.
            // Every exit path from the dispatch loop below yields an outcome too
            // (Stop mid-round, a cancelled approval wait, a dropped dispatch), so
            // a round the loop persists normally is replay-valid. What is NOT
            // covered is the insert itself: if that batch fails, no result row is
            // written at all — see the comment at the batch for the consequence.
            let mut aborted_mid_tools = false;
            let mut outcomes: Vec<DispatchedTool> = Vec::with_capacity(acc.tool_calls.len());
            for call in &acc.tool_calls {
                // YOLO mode treats a gated call as ungated: it runs immediately
                // and is announced with `gated: false` so the UI never renders
                // (or waits on) the approve/deny prompt.
                let host_gated = tools::is_gated(&call.name);
                let yolo = cfg.auto_approve_tools;
                sink.emit(StreamEvent::ToolCall {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.to_string(),
                    gated: host_gated && !yolo,
                });

                // Stop reached this call before it was dispatched (an earlier
                // call's dispatch was cancelled, or the stop landed between the
                // rounds). It still owes the transcript a result row.
                if token.is_cancelled() {
                    aborted_mid_tools = true;
                    outcomes.push(DispatchedTool::plain(aborted_tool_output()));
                    continue;
                }

                let is_memory_tool = matches!(
                    call.name.as_str(),
                    "save_memory" | "read_memory" | "write_memory" | "memory_str_replace"
                );
                // Live, reflection and extraction share one tool list (cache
                // alignment), so access is enforced here by mode: live turns
                // may use every memory tool plus host/web tools, reflection
                // turns may not touch host/web tools, and extraction turns may
                // run no tools at all.
                let refused = match mode {
                    ToolMode::Reflection if !is_memory_tool => Some(format!(
                        "{} is not available during memory consolidation; use save_memory, \
                         read_memory, write_memory, or memory_str_replace.",
                        call.name
                    )),
                    // Extraction advertises no tools, so a hallucinated call is
                    // refused here rather than reaching `dispatch_tool`.
                    ToolMode::Extract => Some(format!(
                        "{} is not available during memory extraction; reply with the summary \
                         text instead of calling tools.",
                        call.name
                    )),
                    _ => None,
                };
                let outcome = if let Some(message) = refused {
                    DispatchedTool::plain(ToolOutput::err(message))
                } else {
                    // A gated call is auto-approved under YOLO, skipped entirely
                    // otherwise (ungated), or parked on the approval channel.
                    let mut approved = !host_gated || yolo;
                    // Stop during the approval wait is an abort, not a Deny:
                    // `stop_chat` cancels *and* denies, so leaving `approved`
                    // false here would record a denial the user never made.
                    let mut aborted_wait = false;
                    if host_gated && !yolo {
                        let mut rx = approvals.register(call.id.clone());
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => aborted_wait = true,
                            verdict = &mut rx => approved = verdict.unwrap_or(false),
                        }
                    }
                    if aborted_wait {
                        aborted_mid_tools = true;
                        DispatchedTool::plain(aborted_tool_output())
                    } else if approved {
                        // The dispatch is raced against cancellation so a running
                        // `bash` (60 s) or `web_fetch` (30 s) is dropped at once;
                        // dropping the future reaps the child (`kill_on_drop`).
                        let dispatched = tokio::select! {
                            biased;
                            _ = token.cancelled() => None,
                            d = dispatch_tool(call, brave, shell, memory, cfg, persist) => Some(d),
                        };
                        match dispatched {
                            Some(d) => d,
                            None => {
                                // The call ran but was stopped before it produced
                                // output: keep the transcript consistent and let
                                // the UI stop showing it as running.
                                aborted_mid_tools = true;
                                DispatchedTool::plain(aborted_tool_output())
                            }
                        }
                    } else {
                        DispatchedTool::plain(ToolOutput {
                            content: "The user denied this tool call.".into(),
                            is_error: true,
                        })
                    }
                };
                // Emit this call's result now, while it is the freshest thing
                // that happened: a round's calls are dispatched one after
                // another, so deferring the emit to the persist loop would keep
                // every card `running` until the slowest call returned (and
                // until a gated call was approved). Persistence below is still
                // deferred — one row per call, written once the whole round has
                // been dispatched — so this moves only the live event.
                sink.emit(StreamEvent::ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    ok: !outcome.output.is_error,
                    output: outcome.output.content.clone(),
                });
                outcomes.push(outcome);
            }

            // Stop may also land just after the round's last dispatch, in which
            // case no outcome is aborted but the turn still ends as aborted.
            aborted_mid_tools |= token.is_cancelled();

            // Images loaded by `read_file` are delivered after every tool result
            // in the round, so the request never interleaves a user turn between
            // an assistant `tool_calls` message and its tool responses.
            let mut injected_images: Vec<ChatCompletionRequestMessage> = Vec::new();
            let mut result_contents: Vec<String> = Vec::with_capacity(acc.tool_calls.len());
            let mut result_texts: Vec<String> = Vec::with_capacity(acc.tool_calls.len());
            let mut images: Vec<(usize, String)> = Vec::new();
            for (pos, (call, dispatched)) in acc.tool_calls.iter().zip(outcomes).enumerate() {
                let DispatchedTool {
                    output,
                    image,
                    attachment,
                } = dispatched;
                if let Some(attachment) = attachment {
                    attachments.push(attachment);
                }
                result_contents.push(encode_tool_result(&call.id, &call.name, &output));
                result_texts.push(output.content);
                if let Some(data_url) = image {
                    images.push((pos, data_url));
                }
            }

            // Persist the round's result rows as ONE batch, before building any
            // of the fallible request messages. A batch is all-or-nothing, so no
            // failure can leave a round half-written.
            //
            // Two residual holes, stated so nobody assumes more:
            //  * The round's assistant `tool_calls` row was already persisted
            //    before dispatch. If this batch fails, that row is left without
            //    results: the turn fails here (the error propagates; no final
            //    assistant row is written) rather than continuing as if the round
            //    were saved, but `build_history_messages` does not detect the
            //    shape, so the next request would be rejected provider-side
            //    (HTTP 400). Practically unreachable, since every field is set.
            //  * Nothing below can desync the transcript: the tool rows are
            //    committed first, so a (practically unreachable) builder error
            //    only ends the turn.
            if let Some(db) = persist {
                db::insert_tool_results(
                    db,
                    source,
                    Some(cfg.model.clone()),
                    Some(cfg.base_url.clone()),
                    Some(cfg.thinking_level.clone()),
                    &result_contents,
                )?;
            }
            for (pos, call) in acc.tool_calls.iter().enumerate() {
                messages.push(
                    ChatCompletionRequestToolMessageArgs::default()
                        .content(result_texts[pos].clone())
                        .tool_call_id(call.id.clone())
                        .build()
                        .map_err(|e| e.to_string())?
                        .into(),
                );
            }
            for (pos, data_url) in images {
                // A `read_file` on an image hands the model a synthetic user
                // image part (tool messages are text-only). It is request-scoped:
                // never persisted, so it does not appear as a turn or replay.
                injected_images.push(image_request_message(&acc.tool_calls[pos], data_url)?);
            }

            if aborted_mid_tools {
                stop_reason = "aborted".into();
                final_thinking = None; // reasoning was attached to the round's rows
                final_text = String::new();
                break;
            }
            messages.extend(injected_images);
            continue;
        }

        // Final answer round.
        final_text = acc.text;
        final_thinking = (!acc.thinking.is_empty()).then_some(acc.thinking);
        stop_reason = acc.finish.unwrap_or_else(|| "stop".to_string());
        break;
    }

    Ok(ToolLoopResult {
        usage: usage_total,
        text: final_text,
        thinking: final_thinking,
        stop_reason,
        error: final_error,
        attachments,
    })
}

/// The live chat turn: persist the user message, run the capped tool loop with
/// DB persistence, freeze the turn's price, and store the final assistant turn.
///
/// When `regenerate` is true the user message is assumed to already exist (the
/// `edit_user_message` path), so no `user` row is inserted; the turn is built
/// from `content` all the same.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_chat_turn(
    db: &db::Db,
    cfg: &AppConfig,
    api_key: &str,
    brave: &tools::BraveSearch,
    shell: &ShellExecutor,
    memory: &crate::memory::MemoryState,
    approvals: &tools::ApprovalRegistry,
    token: CancelToken,
    sink: &dyn EventSink,
    conversation_id: String,
    content: String,
    attachments: Vec<crate::attachments::Attachment>,
    regenerate: bool,
) -> Result<(), String> {
    let conversation = db::get_conversation(db, &conversation_id)?
        .ok_or_else(|| format!("conversation not found: {conversation_id}"))?;
    let mut history = db::read_messages(db, &conversation_id)?;

    // Resolve this conversation's provider and model once, falling back to the
    // active provider and the app's configured model when unset. The override is
    // applied by cloning the config and overriding base_url/model, so
    // `run_tool_loop` (which reads `cfg.base_url`/`cfg.model`) stays unchanged
    // and the reflection pass is untouched.
    let provider = cfg.provider_for(conversation.provider_id.as_deref().unwrap_or(""));
    let model = conversation
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| cfg.model.clone());
    let mut turn_cfg = cfg.clone();
    turn_cfg.base_url = provider.base_url.clone();
    turn_cfg.model = model.clone();

    // Clock note (persisted, hidden from the timeline): written *before* the user
    // turn so it replays verbatim on every later turn and each request stays an
    // exact extension of the previous one (see `clock_note_message`). On the
    // `regenerate` path the edited user row already exists, so the note lands just
    // after it in storage — the one turn whose stored order differs from what was
    // sent, so only that turn's prefix is not strictly extendable.
    let note_text = clock_note_text(crate::db::now_ms());
    db::insert_message(
        db,
        conversation_id.clone(),
        "clock".into(),
        note_text.clone(),
        None,
        None,
        None,
        None,
        None,
        None,
    )?;

    // Persist the incoming user turn before streaming so it survives an abort/error.
    //
    // `regenerate` is the edit path: `apply_user_message_edit` already updated the
    // existing `user` row in place, so inserting another here would duplicate the
    // turn. Instead, drop the (now last) user row from the history we send and
    // rebuild the request from `content` — exactly one copy of the turn goes
    // upstream, and no new row is persisted. A normal send has no such row yet.
    if regenerate {
        // `apply_user_message_edit` truncated everything after the edited row, so
        // it is the last persisted message. Match it by content at the last
        // `user` position rather than blindly popping the tail: if anything were
        // appended in the interim (e.g. a `memory` note), popping the tail would
        // miss and the turn would be sent upstream twice.
        if let Some(pos) = history
            .iter()
            .rposition(|m| m.role == "user" && m.content == content)
        {
            history.remove(pos);
        } else if history.last().is_some_and(|m| m.role == "user") {
            // Defensive fallback: the row we expect is still the last message.
            history.pop();
        }
    } else {
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
            Some(turn_cfg.model.clone()),
            Some(turn_cfg.base_url.clone()),
            // Record the level this turn actually runs at (a snapshot of the
            // global setting at this moment), so a later pass can replay it
            // instead of trusting whatever the global level is by then.
            Some(turn_cfg.thinking_level.clone()),
            None,
            None,
            None,
            attachments_json,
        )?;
    }

    let system_prompt = conversation_system_prompt(
        db,
        cfg,
        &conversation,
        &turn_cfg.base_url,
        &turn_cfg.model,
        memory,
    );

    // The note row was written above, before the user turn; the request still
    // appends it explicitly because `history` was read before it existed. It must
    // survive the compaction rebuild below.
    let time_note = clock_note_message(&note_text)?;

    // Context layout: system prompt → compaction summary (when one exists) →
    // verbatim history → [time note] → the new turn. If the request would cross
    // the threshold of the model's window, force a compaction and rebuild from
    // its result.
    let (mut summary, mut keep_start) = compacted_history(&conversation, &history);
    let mut messages = {
        let mut msgs = build_context(&system_prompt, summary.as_deref(), &history[keep_start..])?;
        msgs.push(time_note.clone());
        msgs.push(user_request_message(&content, &attachments)?);
        msgs
    };
    // The bundled context-window table applies only to catalog providers, and it
    // is the *conversation's* provider that decides — not the global active one.
    let use_bundled_context = crate::pricing::uses_catalog(&provider);
    let window = crate::pricing::resolve_for_base(
        db,
        &turn_cfg.base_url,
        &turn_cfg.model_overrides,
        &turn_cfg.model,
        use_bundled_context,
    )
    .context_window;
    // Prefer the provider's concrete count for everything already sent and add
    // only the *new* turn, which it cannot have reported yet. Falling back to the
    // whole-request estimate on the first turn. Measuring the growth rather than
    // taking a `.max()` keeps the fixed per-image constant from overriding a real
    // API number on an image-heavy conversation (which is what made compaction
    // fire early), while still accounting for a large new turn.
    let current = match reported_context_tokens(&history) {
        Some(reported) => reported + estimate_turn_tokens(&content, &attachments),
        None => estimate_context_tokens(&messages),
    };
    if window > 0 && current as f64 >= window as f64 * COMPACTION_THRESHOLD {
        if let Some((new_summary, new_start)) = compact_now(
            db,
            &conversation,
            &history,
            &turn_cfg,
            use_bundled_context,
            api_key,
            &token,
        )
        .await?
        {
            summary = Some(new_summary);
            keep_start = new_start;
            messages = {
                let mut msgs = build_context(&system_prompt, summary.as_deref(), &history[keep_start..])?;
                msgs.push(time_note.clone());
                msgs.push(user_request_message(&content, &attachments)?);
                msgs
            };
        }
    }

    let tools_list = chat_tools();

    let mut result = run_tool_loop(
        &turn_cfg,
        api_key,
        brave,
        shell,
        memory,
        approvals,
        &conversation_id,
        messages,
        &tools_list,
        token,
        sink,
        Some(db),
        ToolMode::Live,
    )
    .await?;

    approvals.deny_all();

    // Freeze the turn's price at the model/rates in effect now, so switching
    // models later never retroactively re-prices turns already billed.
    if result.usage.get("prompt_tokens").is_some()
        || result.usage.get("completion_tokens").is_some()
    {
        let pricing = crate::pricing::resolve_for_base(
            db,
            &turn_cfg.base_url,
            &turn_cfg.model_overrides,
            &turn_cfg.model,
            use_bundled_context,
        );
        if let Some(cost) = crate::pricing::cost_of_usage(&result.usage, &pricing) {
            result.usage["cost"] = json!(cost);
        }
    }

    // Images the assistant fetched during the turn ride its reply, so they show
    // in the transcript and survive a reload (assistant rows already render
    // attachments). They are collected regardless of how the turn ended.
    let attachments_json = if result.attachments.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&result.attachments).map_err(|e| e.to_string())?)
    };
    db::insert_message_full(
        db,
        conversation_id.clone(),
        "assistant".into(),
        result.text.clone(),
        Some(turn_cfg.model.clone()),
        Some(turn_cfg.base_url.clone()),
        Some(turn_cfg.thinking_level.clone()),
        result.thinking,
        Some(result.usage.to_string()),
        Some(result.stop_reason.clone()),
        attachments_json,
    )?;

    sink.emit(StreamEvent::Done {
        stop_reason: result.stop_reason,
    });
    Ok(())
}

/// Stream an assistant reply for the given conversation: read config + key,
/// register the cancel flag, and hand off to the tool-loop core.
///
/// `regenerate` is `true` after `edit_user_message`: the edited user row already
/// exists, so the turn must not insert a second one.
#[tauri::command]
pub async fn stream_chat(
    app: tauri::AppHandle,
    conversation_id: String,
    content: String,
    attachments: Option<Vec<crate::attachments::Attachment>>,
    regenerate: bool,
    channel: Channel<StreamEvent>,
) -> Result<(), String> {
    let config = app.state::<ConfigState>().get();
    let db = app.state::<db::Db>();

    let conversation = db::get_conversation(&db, &conversation_id)?
        .ok_or_else(|| format!("conversation not found: {conversation_id}"))?;
    let provider = config.provider_for(conversation.provider_id.as_deref().unwrap_or(""));
    let api_key = crate::providers::resolve(&provider.id)?
        .ok_or_else(|| "API key not set for this provider — set it in Settings.".to_string())?;
    // A conversation may pin its own model; fall back to the global one. Guard on
    // the actual model the turn will use, not just the global `config.model`.
    let model = conversation
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| config.model.clone());
    if model.trim().is_empty() {
        return Err("No model selected — set a model in Settings.".into());
    }

    let approvals = app.state::<tools::ApprovalRegistry>();
    let brave = app.state::<tools::BraveSearch>();
    let shell = app.state::<ShellExecutor>();
    let memory = app.state::<crate::memory::MemoryState>();

    let token = app
        .state::<StreamRegistry>()
        .register(conversation_id.clone());
    let sink = ChannelSink(&channel);

    let result = run_chat_turn(
        &db,
        &config,
        &api_key,
        &brave,
        &shell,
        &memory,
        &approvals,
        // A clone shares the same cancellation state; the original is kept so the
        // tail can remove this turn's registration without touching one a resend
        // may have installed in the meantime.
        token.clone(),
        &sink,
        conversation_id.clone(),
        content,
        attachments.unwrap_or_default(),
        regenerate,
    )
    .await;

    app.state::<StreamRegistry>().remove(&conversation_id, &token);
    approvals.deny_all();
    result
}

/// Abort a running stream for a conversation, immediately: in-flight requests
/// and tool calls are dropped (a running `bash` child is reaped), partial text is
/// preserved, and the turn finishes with `stop_reason = "aborted"`.
#[tauri::command]
pub fn stop_chat(app: tauri::AppHandle, conversation_id: String) -> Result<(), String> {
    app.state::<StreamRegistry>().cancel(&conversation_id);
    app.state::<tools::ApprovalRegistry>().deny_all();
    Ok(())
}

#[cfg(test)]
mod tests;
