    use super::*;
    use crate::config::AppConfig;
    use crate::db::Db;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    fn temp_db_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-loop-{name}-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_db(name: &str) -> Db {
        let conn = crate::db::open(&temp_db_path(name)).unwrap();
        Db(std::sync::Arc::new(std::sync::Mutex::new(conn)))
    }

    fn temp_memory(name: &str) -> crate::memory::MemoryState {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-loop-memory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        crate::memory::MemoryState::load(p).unwrap()
    }

    fn insert_conv(db: &Db) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?1, 'test', 0, 0)",
            [&id],
        )
        .unwrap();
        id
    }

    fn msgs(db: &Db, cid: &str) -> Vec<crate::db::MessageRow> {
        crate::db::read_messages(db, cid).unwrap()
    }

    struct TestSink(Arc<Mutex<Vec<StreamEvent>>>);

    impl EventSink for TestSink {
        fn emit(&self, ev: StreamEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    /// A sink that trips the cancel flag as soon as the first delta lands, so an
    /// abort can be triggered deterministically mid-stream without a race.
    struct FlagOnDelta {
        flag: Arc<AtomicBool>,
        events: Arc<Mutex<Vec<StreamEvent>>>,
    }

    impl EventSink for FlagOnDelta {
        fn emit(&self, ev: StreamEvent) {
            if matches!(&ev, StreamEvent::Delta { .. }) {
                self.flag.store(true, Ordering::SeqCst);
            }
            self.events.lock().unwrap().push(ev);
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Read a full HTTP request (headers + content-length body) off the socket,
    /// returning the raw request text.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = stream.read(&mut tmp).unwrap_or(0);
            if n == 0 {
                return String::from_utf8_lossy(&buf).into_owned();
            }
            buf.extend_from_slice(&tmp[..n]);
            if find(&buf, b"\r\n\r\n").is_some() {
                break;
            }
        }
        let Some(header_end) = find(&buf, b"\r\n\r\n").map(|p| p + 4) else {
            return String::from_utf8_lossy(&buf).into_owned();
        };
        let head = String::from_utf8_lossy(&buf[..header_end]);
        let len = head
            .lines()
            .find_map(|l| {
                let low = l.to_ascii_lowercase();
                low.strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
            })
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

    /// Parse the JSON body of a raw HTTP request captured by the mock server.
    fn request_body(request: &str) -> Value {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or(request);
        serde_json::from_str(body).unwrap()
    }

    /// A mock OpenAI-compatible SSE endpoint. Each `turn` is served on its own
    /// connection: one `data:` event per string, optionally spaced by `gap_ms`
    /// so a test can abort between chunks. Always closes with `[DONE]`.
    fn start_mock(turns: Vec<Vec<String>>, gap_ms: u64) -> (u16, JoinHandle<()>) {
        let (port, _bodies, handle) = start_mock_capture(turns, gap_ms);
        (port, handle)
    }

    /// Like [`start_mock`], but also records every request body seen (headers
    /// included) so tests can assert on what was sent upstream.
    fn start_mock_capture(
        turns: Vec<Vec<String>>,
        gap_ms: u64,
    ) -> (u16, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let bodies2 = bodies.clone();
        let handle = std::thread::spawn(move || {
            for events in turns {
                let (mut stream, _) = match listener.accept() {
                    Ok(x) => x,
                    Err(_) => return,
                };
                let request = read_request(&mut stream);
                bodies2.lock().unwrap().push(request);
                let chunks: Vec<String> = events
                    .iter()
                    .map(|ev| format!("data: {ev}\n\n"))
                    .chain(std::iter::once("data: [DONE]\n\n".to_string()))
                    .collect();
                let total_len: usize = chunks.iter().map(|c| c.len()).sum();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total_len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.flush();
                // Write the SSE body progressively so a test can abort between
                // events; each chunk is flushed before an optional gap.
                for (i, chunk) in chunks.iter().enumerate() {
                    if stream.write_all(chunk.as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    if gap_ms > 0 && i + 1 < chunks.len() {
                        std::thread::sleep(Duration::from_millis(gap_ms));
                    }
                }
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        (port, bodies, handle)
    }

    /// Serve one raw SSE body verbatim — no implicit `[DONE]` and no trailing
    /// blank line — so framing edge cases can be exercised.
    fn start_mock_raw(body: String) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            let _ = read_request(&mut stream);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        });
        (port, handle)
    }

    fn delta_chunk(content: &str) -> String {
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{"role":"assistant","content":"{content}"}},"finish_reason":null}}]}}"#
        )
    }

    fn reasoning_chunk(reasoning: &str) -> String {
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{"reasoning_content":"{reasoning}"}},"finish_reason":null}}]}}"#
        )
    }

    /// Reasoning aliased as a plain `thinking` string delta field (some proxies).
    fn thinking_alias_chunk(reasoning: &str) -> String {
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{"thinking":"{reasoning}"}},"finish_reason":null}}]}}"#
        )
    }

    /// A content-array reasoning part carrying `content` (Responses style).
    fn content_part_reasoning_chunk(reasoning: &str) -> String {
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{"content":[{{"type":"reasoning","content":["{reasoning}"]}}]}},"finish_reason":null}}]}}"#
        )
    }

    fn finish_chunk(reason: &str) -> String {
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{}},"finish_reason":"{reason}"}}]}}"#
        )
    }

    fn usage_chunk() -> String {
        r#"{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#.into()
    }

    fn tool_frag(index: u32, id: &str, name: &str, args: &str) -> String {
        let id_json = if id.is_empty() { "null".to_string() } else { format!("\"{id}\"") };
        let name_json = if name.is_empty() { "null".to_string() } else { format!("\"{name}\"") };
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":{index},"id":{id_json},"type":"function","function":{{"name":{name_json},"arguments":"{args}"}}}}]}},"finish_reason":null}}]}}"#
        )
    }

    /// InferX-style tool-call delta: restates the whole `tool_calls` array on
    /// every fragment chunk, blanking `id`/`function.name` to **explicit empty
    /// strings** rather than omitting them (which is what OpenAI's spec does).
    /// The first chunk alone carries the real id/name.
    fn tool_frag_inferx(index: u32, id: &str, name: &str, args: &str) -> String {
        format!(
            r#"{{"id":"1","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{{"index":0,"delta":{{"tool_calls":[{{"index":{index},"id":"{id}","type":"function","function":{{"name":"{name}","arguments":"{args}"}}}}]}},"finish_reason":null}}]}}"#
        )
    }

    async fn wait_for_event(
        events: &Arc<Mutex<Vec<StreamEvent>>>,
        pred: impl Fn(&StreamEvent) -> bool,
        timeout_ms: u64,
    ) {
        let start = Instant::now();
        loop {
            {
                let guard = events.lock().unwrap();
                if guard.iter().any(&pred) {
                    return;
                }
            }
            if start.elapsed() > Duration::from_millis(timeout_ms) {
                panic!("timed out waiting for stream event");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn complete_once(port: u16) -> RoundAccum {
        let base = format!("http://127.0.0.1:{port}/v1");
        let body = completion_body(
            "mock",
            &build_messages("p", None, &[], "hi", &[]).unwrap(),
            &tools::tool_specs(false),
            "",
            None,
        )
        .unwrap();
        let resp = open_completion_stream(&base, "k", &body).await.unwrap();
        let sink = TestSink(Arc::new(Mutex::new(Vec::new())));
        run_completion(resp, &sink, &AtomicBool::new(false)).await
    }

    /// Like [`complete_once`], but also returns the emitted events.
    async fn complete_events(port: u16) -> (RoundAccum, Arc<Mutex<Vec<StreamEvent>>>) {
        let base = format!("http://127.0.0.1:{port}/v1");
        let body = completion_body(
            "mock",
            &build_messages("p", None, &[], "hi", &[]).unwrap(),
            &tools::tool_specs(false),
            "",
            None,
        )
        .unwrap();
        let resp = open_completion_stream(&base, "k", &body).await.unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let acc = run_completion(resp, &sink, &AtomicBool::new(false)).await;
        (acc, events)
    }

    /// Stream an arbitrary raw SSE body through `run_completion`.
    async fn complete_raw(body: &str) -> (RoundAccum, Arc<Mutex<Vec<StreamEvent>>>) {
        let (port, handle) = start_mock_raw(body.to_string());
        let base = format!("http://127.0.0.1:{port}/v1");
        let req = completion_body(
            "mock",
            &build_messages("p", None, &[], "hi", &[]).unwrap(),
            &tools::tool_specs(false),
            "",
            None,
        )
        .unwrap();
        let resp = open_completion_stream(&base, "k", &req).await.unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let acc = run_completion(resp, &sink, &AtomicBool::new(false)).await;
        let _ = handle.join();
        (acc, events)
    }

    // ---------- parser tests ----------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_emits_deltas_and_usage() {
        let (port, handle) = start_mock(
            vec![vec![
                delta_chunk("Hello"),
                delta_chunk(" world"),
                finish_chunk("stop"),
                usage_chunk(),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert!(!acc.failed);
        assert_eq!(acc.text, "Hello world");
        assert_eq!(acc.finish.as_deref(), Some("stop"));
        assert_eq!(
            acc.usage.unwrap()["total_tokens"],
            serde_json::json!(5)
        );
        let _ = handle.join();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_captures_reasoning_field() {
        let (port, handle) = start_mock(
            vec![vec![
                reasoning_chunk("Let me think. "),
                delta_chunk("Hello"),
                finish_chunk("stop"),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert_eq!(acc.text, "Hello");
        assert_eq!(acc.thinking, "Let me think. ");
        let _ = handle.join();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_captures_reasoning_aliases() {
        // A provider that streams reasoning as a plain `thinking` delta field.
        let (port, handle) = start_mock(
            vec![vec![
                thinking_alias_chunk("Let me think. "),
                delta_chunk("Hello"),
                finish_chunk("stop"),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert_eq!(acc.text, "Hello");
        assert_eq!(acc.thinking, "Let me think. ");
        let _ = handle.join();

        // A Responses-style content-array reasoning part carrying `content`.
        let (port, handle) = start_mock(
            vec![vec![
                content_part_reasoning_chunk("Surely. "),
                delta_chunk("Hi"),
                finish_chunk("stop"),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert_eq!(acc.text, "Hi");
        assert_eq!(acc.thinking, "Surely. ");
        let _ = handle.join();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_accumulates_tool_call_fragments() {
        let (port, handle) = start_mock(
            vec![vec![
                tool_frag(0, "call_1", "bash", ""),
                tool_frag(0, "", "", r#"{\"command\":\"ls\"}"#),
                finish_chunk("tool_calls"),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert!(!acc.failed);
        assert!(acc.text.is_empty());
        assert_eq!(acc.finish.as_deref(), Some("tool_calls"));
        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].id, "call_1");
        assert_eq!(acc.tool_calls[0].name, "bash");
        assert_eq!(acc.tool_calls[0].arguments["command"], "ls");
        let _ = handle.join();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_keeps_id_name_across_blank_repeat_chunks() {
        // Some gateways (InferX) restate the tool_calls array on every fragment
        // chunk with id/name blanked to "" rather than omitted; the metadata
        // from the first chunk must survive while arguments still accumulate.
        // Regression for the persisted `{"id":"","name":""}` rows that produced
        // "unknown tool: " in live chats.
        let (port, handle) = start_mock(
            vec![vec![
                tool_frag_inferx(0, "call_9", "bash", r#"{\"command\":\"ls\""#),
                tool_frag_inferx(0, "", "", r#",\"--color\":\"never\"}"#),
                finish_chunk("tool_calls"),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert!(!acc.failed);
        assert!(acc.text.is_empty());
        assert_eq!(acc.finish.as_deref(), Some("tool_calls"));
        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].id, "call_9");
        assert_eq!(acc.tool_calls[0].name, "bash");
        assert_eq!(acc.tool_calls[0].arguments["command"], "ls");
        assert_eq!(acc.tool_calls[0].arguments["--color"], "never");
        let _ = handle.join();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_synthesizes_id_when_absent_across_gateway_repeats() {
        // A gateway that never sends an id at all (null in every chunk, here
        // followed by an InferX-style "" repeat) must still produce the
        // synthesized `call_{idx}` id, and the name from the first chunk must
        // survive the blank repeats.
        let (port, handle) = start_mock(
            vec![vec![
                tool_frag(0, "", "bash", r#"{\"command\":\"ls\""#),
                tool_frag(0, "", "", r#",\"--all\":true}"#),
                tool_frag_inferx(0, "", "", ""),
                finish_chunk("tool_calls"),
            ]],
            0,
        );
        let acc = complete_once(port).await;
        assert!(!acc.failed);
        assert_eq!(acc.finish.as_deref(), Some("tool_calls"));
        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].id, "call_0");
        assert_eq!(acc.tool_calls[0].name, "bash");
        assert_eq!(acc.tool_calls[0].arguments["command"], "ls");
        assert_eq!(acc.tool_calls[0].arguments["--all"], true);
        let _ = handle.join();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_flushes_unterminated_final_event() {
        // The body ends without a trailing blank line and without `[DONE]`; the
        // final delta must still be emitted rather than dropped.
        let body = format!("data: {}", delta_chunk("tail"));
        let (acc, events) = complete_raw(&body).await;
        assert!(!acc.failed);
        assert_eq!(acc.text, "tail");
        let guard = events.lock().unwrap();
        assert!(guard
            .iter()
            .any(|e| matches!(e, StreamEvent::Delta { text } if text == "tail")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_reports_mid_stream_error_and_keeps_partial() {
        let (port, handle) = start_mock(
            vec![vec![
                delta_chunk("Partial"),
                r#"{"error":{"message":"boom","type":"server_error"}}"#.into(),
            ]],
            0,
        );
        let (acc, events) = complete_events(port).await;
        let _ = handle.join();

        assert!(acc.failed);
        assert_eq!(acc.text, "Partial");
        let guard = events.lock().unwrap();
        assert!(guard
            .iter()
            .any(|e| matches!(e, StreamEvent::Error { message } if message.contains("boom"))));
        assert!(guard
            .iter()
            .any(|e| matches!(e, StreamEvent::Delta { text } if text == "Partial")));
    }

    #[test]
    fn apply_chunk_treats_string_error_as_fatal() {
        let mut out = RoundAccum::default();
        let mut calls = HashMap::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let stop = apply_chunk(&mut out, &mut calls, &json!({"error": "plain boom"}), &sink);
        assert!(stop);
        assert!(out.failed);
        let guard = events.lock().unwrap();
        assert!(guard
            .iter()
            .any(|e| matches!(e, StreamEvent::Error { message } if message == "plain boom")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_completion_honors_abort_between_fast_events() {
        // Deltas arrive back-to-back (no gap), so a per-iteration timer would
        // never fire; the flag must be honored between events.
        let (port, handle) = start_mock(
            vec![vec![
                delta_chunk("one"),
                delta_chunk("two"),
                delta_chunk("three"),
                finish_chunk("stop"),
            ]],
            0,
        );
        let base = format!("http://127.0.0.1:{port}/v1");
        let body = completion_body(
            "mock",
            &build_messages("p", None, &[], "hi", &[]).unwrap(),
            &tools::tool_specs(false),
            "",
            None,
        )
        .unwrap();
        let resp = open_completion_stream(&base, "k", &body).await.unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = FlagOnDelta {
            flag: flag.clone(),
            events: events.clone(),
        };
        let acc = run_completion(resp, &sink, &flag).await;
        let _ = handle.join();

        assert!(flag.load(Ordering::SeqCst));
        assert_eq!(acc.text, "one");
        assert_eq!(acc.finish, None);
    }

    #[test]
    fn take_first_event_handles_lf_and_crlf() {
        let mut buf = b"data: {\"a\":1}\n\n".to_vec();
        assert_eq!(take_first_event(&mut buf).as_deref(), Some(r#"{"a":1}"#));
        assert!(buf.is_empty());

        let mut buf = b"data: one\r\ndata: two\r\n\r\n".to_vec();
        assert_eq!(take_first_event(&mut buf).as_deref(), Some("one\ntwo"));
        assert!(buf.is_empty());
    }

    // ---------- chat-loop tests (tool calls, persistence, reasoning) ----------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_persists_user_and_assistant_message() {
        let (port, handle) = start_mock(
            vec![vec![
                delta_chunk("Hello"),
                finish_chunk("stop"),
                usage_chunk(),
            ]],
            0,
        );
        let db = Arc::new(open_db("single"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("single");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid_task, "hi".into(), Vec::new(),
            false,
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[0].content, "hi");
        assert_eq!(rows[1].role, "assistant");
        assert_eq!(rows[1].content, "Hello");
        assert_eq!(rows[1].stop_reason.as_deref(), Some("stop"));
        let usage: Value = serde_json::from_str(rows[1].usage.as_deref().unwrap()).unwrap();
        assert_eq!(usage["total_tokens"], 5);
        // Context size is the last round's prompt+completion (3 + 2).
        assert_eq!(usage["context_tokens"], 5);

        let guard = events.lock().unwrap();
        let texts: Vec<String> = guard
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Delta { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello".to_string()]);
        assert!(matches!(guard.last(), Some(StreamEvent::Done { stop_reason }) if stop_reason == "stop"));
    }

    /// A conversation pinned to a non-active provider + its own model must run the
    /// turn against THAT provider (base URL) and model, not the active one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_uses_conversation_provider_and_model() {
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![
                delta_chunk("Pinned"),
                finish_chunk("stop"),
                usage_chunk(),
            ]],
            0,
        );
        let db = Arc::new(open_db("prov-turn"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = uuid::Uuid::new_v4().to_string();
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations (id, title, provider_id, model, created_at, updated_at)
                 VALUES (?1, 'pinned', 'p-b', 'conv-model', 0, 0)",
                [&cid],
            )
            .unwrap();
        }
        // Active provider points elsewhere; provider "p-b" is the mock endpoint.
        // Two providers so the active one genuinely differs from the pinned one.
        let mock_url = format!("http://127.0.0.1:{port}/v1");
        let cfg = AppConfig {
            base_url: "http://active.invalid/v1".into(),
            model: "mock".into(),
            providers: vec![
                crate::config::Provider {
                    id: "p-a".into(),
                    name: "A".into(),
                    base_url: "http://active.invalid/v1".into(),
                    default_model: None,
                    catalog_id: None,
                },
                crate::config::Provider {
                    id: "p-b".into(),
                    name: "B".into(),
                    base_url: mock_url.clone(),
                    default_model: None,
                    catalog_id: None,
                },
            ],
            active_provider_id: "p-a".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));
        let memory = temp_memory("prov-turn");

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid_task, "ping".into(), Vec::new(),
            false,
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        // The request must have gone to the pinned provider's endpoint...
        assert!(!bodies.lock().unwrap().is_empty(), "no request hit the mock");
        // ...and be stamped with the pinned provider's base URL + model.
        let rows = msgs(&db, &cid);
        let assistant = rows.iter().find(|m| m.role == "assistant").unwrap();
        assert_eq!(assistant.model.as_deref(), Some("conv-model"));
        assert_eq!(assistant.provider.as_deref(), Some(mock_url.as_str()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_sends_preferences_and_model_in_system_prompt() {
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![delta_chunk("ok"), finish_chunk("stop")]],
            0,
        );
        let db = Arc::new(open_db("prompt"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            preferences: "Talk like me.".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("prompt-request");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid_task, "hi".into(),
                Vec::new(),
            false,
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        let request = bodies.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("Explicit user preferences"));
        assert!(request.contains("Talk like me."));
        assert!(request.contains("running as the model"));
        assert!(request.contains("mock"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_persists_and_sends_attachments() {
        use crate::attachments::Attachment;
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![delta_chunk("ok"), finish_chunk("stop")]],
            0,
        );
        let db = Arc::new(open_db("attach"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));
        let attachments = vec![
            Attachment {
                id: "a1".into(),
                name: "notes.txt".into(),
                mime: "text/plain".into(),
                size: 5,
                kind: "file".into(),
                data_url: None,
                text: Some("hello".into()),
            },
            Attachment {
                id: "a2".into(),
                name: "pic.png".into(),
                mime: "image/png".into(),
                size: 4,
                kind: "image".into(),
                data_url: Some("data:image/png;base64,AAECAw==".into()),
                text: None,
            },
        ];

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("attach");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid_task, "look".into(), attachments,
            false,
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        // Persisted on the user row.
        let rows = msgs(&db, &cid);
        let stored: Value =
            serde_json::from_str(rows[0].attachments.as_deref().unwrap()).unwrap();
        assert_eq!(stored[0]["name"], "notes.txt");
        assert_eq!(stored[1]["kind"], "image");

        // Sent upstream: text file folded into content, image as a part.
        let request = bodies.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("--- File: notes.txt ---"));
        assert!(request.contains("hello"));
        assert!(request.contains("image_url"));
        assert!(request.contains("AAECAw=="));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_stores_frozen_turn_cost() {
        let (port, handle) = start_mock(
            vec![vec![
                delta_chunk("Hello"),
                finish_chunk("stop"),
                usage_chunk(),
            ]],
            0,
        );
        let db = Arc::new(open_db("cost"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let mut cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        // Give "mock" rates so the turn's cost can be resolved and frozen.
        cfg.model_overrides.insert(
            "mock".into(),
            crate::config::ModelOverride {
                input_per_million: Some(1.0),
                output_per_million: Some(2.0),
                ..Default::default()
            },
        );
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("cost");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid_task, "hi".into(),
                Vec::new(),
            false,
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        let usage: Value = serde_json::from_str(rows[1].usage.as_deref().unwrap()).unwrap();
        // 3 input @ $1/M + 2 output @ $2/M.
        assert!((usage["cost"].as_f64().unwrap() - 7e-6).abs() < 1e-12);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_persists_reasoning_trace_with_assistant_message() {
        let (port, handle) = start_mock(
            vec![vec![
                reasoning_chunk("I should check. "),
                delta_chunk("Answer"),
                finish_chunk("stop"),
            ]],
            0,
        );
        let db = Arc::new(open_db("reasoning"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("reasoning");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid_task, "q".into(), Vec::new(),
            false,
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].thinking.as_deref(), Some("I should check. "));
        assert_eq!(rows[1].content, "Answer");

        let guard = events.lock().unwrap();
        assert!(guard.iter().any(|e| matches!(
            e,
            StreamEvent::ThinkingDelta { text } if text == "I should check. "
        )));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_runs_gated_tool_then_final_answer() {
        let (port, handle) = start_mock(
            vec![
                vec![
                    tool_frag(0, "call_1", "bash", ""),
                    tool_frag(0, "", "", r#"{\"command\":\"echo hi\"}"#),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("All done"), finish_chunk("stop"), usage_chunk()],
            ],
            0,
        );
        let db = Arc::new(open_db("tool"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("tool");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2,
                &cfg,
                "test-key",
                &mcp,
                &shell,
                &memory,
                &approvals2,
                flag,
                &sink,
                cid2,
                "run echo hi".into(),
                Vec::new(),
            false,
            )
            .await
        });

        // Wait until the loop is parked on the gated bash call, then approve.
        wait_for_event(
            &events2,
            |e| matches!(e, StreamEvent::ToolCall { gated: true, name, .. } if name == "bash"),
            5000,
        )
        .await;
        let call_id = {
            let guard = events2.lock().unwrap();
            guard
                .iter()
                .find_map(|e| match e {
                    StreamEvent::ToolCall { call_id, gated: true, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        // Resolve retries until the loop has actually registered the call.
        while !approvals.resolve(&call_id, true) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[1].role, "assistant");
        let tc: Value = serde_json::from_str(&rows[1].content).unwrap();
        assert_eq!(tc["tool_calls"][0]["name"], "bash");
        assert_eq!(rows[2].role, "tool");
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["tool_call_id"], "call_1");
        assert_eq!(tr["error"], false);
        assert_eq!(tr["output"], "hi");
        assert_eq!(rows[3].role, "assistant");
        assert_eq!(rows[3].content, "All done");
        assert_eq!(rows[3].stop_reason.as_deref(), Some("stop"));
        let usage: Value = serde_json::from_str(rows[3].usage.as_deref().unwrap()).unwrap();
        assert_eq!(usage["total_tokens"], 5);

        let guard = events.lock().unwrap();
        assert!(guard.iter().any(|e| matches!(e, StreamEvent::ToolCall { gated: true, .. })));
        assert!(guard.iter().any(|e| matches!(
            e,
            StreamEvent::ToolResult { ok: true, output, .. } if output == "hi"
        )));
        assert!(guard.iter().any(|e| matches!(e, StreamEvent::Delta { text } if text == "All done")));
        assert!(matches!(guard.last(), Some(StreamEvent::Done { stop_reason }) if stop_reason == "stop"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_echoes_reasoning_on_tool_call_message_within_turn() {
        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    reasoning_chunk("I should check. "),
                    tool_frag(0, "call_1", "bash", ""),
                    tool_frag(0, "", "", r#"{\"command\":\"echo hi\"}"#),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("All done"), finish_chunk("stop"), usage_chunk()],
            ],
            0,
        );
        let db = Arc::new(open_db("echo-reasoning"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("echo-reasoning");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid2, "run echo hi".into(), Vec::new(),
            false,
            )
            .await
        });

        wait_for_event(
            &events2,
            |e| matches!(e, StreamEvent::ToolCall { gated: true, name, .. } if name == "bash"),
            5000,
        )
        .await;
        let call_id = {
            let guard = events2.lock().unwrap();
            guard
                .iter()
                .find_map(|e| match e {
                    StreamEvent::ToolCall { call_id, gated: true, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        while !approvals.resolve(&call_id, true) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        task.await.unwrap().unwrap();
        let _ = handle.join();

        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 2);
        // The first request has no tool-call assistant message yet.
        assert!(!requests[0].contains("reasoning_content"));

        let second = request_body(&requests[1]);
        let msgs = second["messages"].as_array().unwrap();
        let tool_call = msgs
            .iter()
            .find(|m| m["role"] == "assistant" && m.get("tool_calls").is_some())
            .expect("assistant tool-call message");
        assert_eq!(tool_call["reasoning_content"], "I should check. ");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_replay_does_not_resend_previous_turn_reasoning() {
        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    reasoning_chunk("I should check. "),
                    tool_frag(0, "call_1", "bash", ""),
                    tool_frag(0, "", "", r#"{\"command\":\"echo hi\"}"#),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("All done"), finish_chunk("stop")],
                vec![delta_chunk("Second"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("echo-replay"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();

        // First turn: a tool round whose reasoning is persisted on the row.
        let flag = Arc::new(AtomicBool::new(false));
        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let cfg2 = cfg.clone();
        let memory = temp_memory("echo-replay");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg2, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid2, "run echo hi".into(), Vec::new(),
            false,
            )
            .await
        });
        wait_for_event(&events2, |e| matches!(e, StreamEvent::ToolCall { gated: true, .. }), 5000)
            .await;
        let call_id = {
            let guard = events2.lock().unwrap();
            guard
                .iter()
                .find_map(|e| match e {
                    StreamEvent::ToolCall { call_id, gated: true, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        while !approvals.resolve(&call_id, true) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        task.await.unwrap().unwrap();

        // Second turn: a fresh user message replays the tool-call history.
        let flag2 = Arc::new(AtomicBool::new(false));
        let sink2 = TestSink(events.clone());
        let memory2 = temp_memory("echo-replay-2");
        let mcp2 = tools::BraveSearch::default();
        let shell2 = crate::shell::ShellExecutor::new();
        run_chat_turn(
            &db, &cfg, "test-key", &mcp2, &shell2, &memory2, &approvals, flag2, &sink2,
            cid.clone(), "again".into(), Vec::new(),
        false,
        )
        .await
        .unwrap();
        let _ = handle.join();

        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let replayed = request_body(&requests[2]);
        let msgs = replayed["messages"].as_array().unwrap();
        // The tool-call row is replayed, but without its reasoning trace.
        assert!(msgs
            .iter()
            .any(|m| m["role"] == "assistant" && m.get("tool_calls").is_some()));
        assert!(!replayed.to_string().contains("reasoning_content"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_reasoning_echo_can_be_disabled() {
        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    reasoning_chunk("I should check. "),
                    tool_frag(0, "call_1", "bash", ""),
                    tool_frag(0, "", "", r#"{\"command\":\"echo hi\"}"#),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("All done"), finish_chunk("stop"), usage_chunk()],
            ],
            0,
        );
        let db = Arc::new(open_db("echo-off"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            echo_reasoning_content: false,
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("echo-off");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid2, "run echo hi".into(), Vec::new(),
            false,
            )
            .await
        });

        wait_for_event(
            &events2,
            |e| matches!(e, StreamEvent::ToolCall { gated: true, name, .. } if name == "bash"),
            5000,
        )
        .await;
        let call_id = {
            let guard = events2.lock().unwrap();
            guard
                .iter()
                .find_map(|e| match e {
                    StreamEvent::ToolCall { call_id, gated: true, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        while !approvals.resolve(&call_id, true) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        task.await.unwrap().unwrap();
        let _ = handle.join();

        let requests = bodies.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let second = request_body(&requests[1]);
        let msgs = second["messages"].as_array().unwrap();
        assert!(msgs
            .iter()
            .any(|m| m["role"] == "assistant" && m.get("tool_calls").is_some()));
        assert!(!second.to_string().contains("reasoning_content"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_denied_tool_reports_error_and_keeps_going() {
        let (port, handle) = start_mock(
            vec![
                vec![tool_frag(0, "call_1", "bash", ""), finish_chunk("tool_calls")],
                vec![delta_chunk("Never mind"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("deny"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("deny");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid2, "do it".into(), Vec::new(),
            false,
            )
            .await
        });

        wait_for_event(&events2, |e| matches!(e, StreamEvent::ToolCall { gated: true, .. }), 5000)
            .await;
        let call_id = {
            let guard = events2.lock().unwrap();
            guard
                .iter()
                .find_map(|e| match e {
                    StreamEvent::ToolCall { call_id, gated: true, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        while !approvals.resolve(&call_id, false) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 4);
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], true);
        assert_eq!(tr["output"], "The user denied this tool call.");
        assert_eq!(rows[3].content, "Never mind");

        let guard = events.lock().unwrap();
        assert!(guard.iter().any(|e| matches!(e, StreamEvent::ToolResult { ok: false, .. })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_executes_tool_despite_blank_repeat_chunks() {
        // End-to-end regression for the InferX wire shape: round 1 streams a
        // bash call whose id/name appear only on the first chunk ("" on the
        // repeats). The tool must still execute and round 2 must persist —
        // previously this landed as `{"id":"","name":""}` → "unknown tool: ".
        let (port, handle) = start_mock(
            vec![
                vec![
                    tool_frag_inferx(0, "call_7", "bash", ""),
                    tool_frag_inferx(0, "", "", r#"{\"command\":\"echo inferx-ran\"}"#),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("Ran it"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("inferx"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("inferx");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid2, "run ls".into(), Vec::new(),
            false,
            )
            .await
        });

        wait_for_event(&events2, |e| matches!(e, StreamEvent::ToolCall { gated: true, .. }), 5000)
            .await;
        let call_id = {
            let guard = events2.lock().unwrap();
            guard
                .iter()
                .find_map(|e| match e {
                    StreamEvent::ToolCall { call_id, gated: true, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .unwrap()
        };
        while !approvals.resolve(&call_id, true) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 4);
        // The persisted assistant tool-call row must carry the real id and name.
        let tcv: Value = serde_json::from_str(&rows[1].content).unwrap();
        assert_eq!(tcv["tool_calls"][0]["name"], "bash");
        assert_eq!(tcv["tool_calls"][0]["id"], "call_7");
        // The tool result must be a successful real execution, not an error.
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], false);
        assert_eq!(tr["name"], "bash");
        assert_eq!(tr["output"], "inferx-ran");
        assert_eq!(rows[3].content, "Ran it");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loop_abort_preserves_partial_text() {
        let (port, handle) = start_mock(
            vec![vec![delta_chunk("Hello"), delta_chunk(" world")]],
            800,
        );
        let db = Arc::new(open_db("abort"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let flag2 = flag.clone();
        let cid2 = cid.clone();
        let memory = temp_memory("abort");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag2, &sink, cid2, "say something".into(), Vec::new(),
            false,
            )
            .await
        });

        // Stop as soon as the first delta has been delivered, before " world".
        wait_for_event(
            &events2,
            |e| matches!(e, StreamEvent::Delta { text } if text == "Hello"),
            5000,
        )
        .await;
        flag.store(true, Ordering::SeqCst);

        task.await.unwrap().unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].content, "Hello");
        assert_eq!(rows[1].stop_reason.as_deref(), Some("aborted"));
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(StreamEvent::Done { stop_reason }) if stop_reason == "aborted"
        ));
    }

/// The extracted loop used by the reflection pass runs without a DB and still
/// executes memory tools (writes land in the memory dir, nothing persisted).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_loop_without_persistence_runs_memory_tool() {
    let (port, handle) = start_mock(
        vec![
            vec![
                tool_frag(0, "call_1", "save_memory", ""),
                tool_frag(
                    0,
                    "",
                    "",
                    r#"{\"content\":\"User likes tea.\",\"path\":\"profile.md\"}"#,
                ),
                finish_chunk("tool_calls"),
            ],
            vec![delta_chunk("noted"), finish_chunk("stop"), usage_chunk()],
        ],
        0,
    );
    let cfg = AppConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "mock".into(),
        ..Default::default()
    };
    let mcp = tools::BraveSearch::default();
    let shell = crate::shell::ShellExecutor::new();
    let memory = temp_memory("loop-nopersist");
    let approvals = tools::ApprovalRegistry::default();
    let flag = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = TestSink(events.clone());

    let messages = build_messages("p", None, &[], "remember I like tea", &[]).unwrap();
    let tools_list = tools::tool_specs(false);
    let result = run_tool_loop(
        &cfg,
        "test-key",
        &mcp,
        &shell,
        &memory,
        &approvals,
        "conv_reflect",
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        ToolMode::Reflection,
    )
    .await
    .unwrap();
    let _ = handle.join();

    assert_eq!(result.stop_reason, "stop");
    assert_eq!(result.text, "noted");
    assert_eq!(result.usage["total_tokens"], 5);
    assert!(memory.read("profile.md").unwrap().contains("User likes tea."));
    let guard = events.lock().unwrap();
    assert!(guard
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolResult { ok: true, .. })));
}

/// `write_memory` is usable during both a live chat turn and a reflection pass:
/// the live turn must no longer be refused (the chatting model shares the same
/// memory tools), and it must actually replace the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_memory_is_allowed_live_and_in_reflection() {
    async fn run(mode: ToolMode, memory: &crate::memory::MemoryState) -> Arc<Mutex<Vec<StreamEvent>>> {
        let (port, handle) = start_mock(
            vec![
                vec![
                    tool_frag(0, "c1", "write_memory", ""),
                    tool_frag(
                        0,
                        "",
                        "",
                        r#"{\"path\":\"profile.md\",\"content\":\"REWRITTEN\"}"#,
                    ),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("done"), finish_chunk("stop")],
            ],
            0,
        );
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let mcp = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let approvals = tools::ApprovalRegistry::default();
        let flag = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let messages = build_messages("p", None, &[], "hi", &[]).unwrap();
        let tools_list = tools::tool_specs(false);
        run_tool_loop(
            &cfg,
            "test-key",
            &mcp,
            &shell,
            memory,
            &approvals,
            "conv",
            messages,
            &tools_list,
            flag,
            &sink,
            None,
            mode,
        )
        .await
        .unwrap();
        let _ = handle.join();
        events
    }

    // Live: allowed now — the file is replaced, no refusal.
    let live_memory = temp_memory("wm-live");
    let live_events = run(ToolMode::Live, &live_memory).await;
    assert_eq!(live_memory.read("profile.md").unwrap(), "REWRITTEN");
    {
        let guard = live_events.lock().unwrap();
        assert!(guard
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolResult { ok: true, .. })));
        assert!(!guard.iter().any(|e| matches!(
            e,
            StreamEvent::ToolResult { ok: false, output, .. }
                if output.contains("memory consolidation")
        )));
    }

    // Reflection: allowed, file is replaced.
    let reflect_memory = temp_memory("wm-reflect");
    let reflect_events = run(ToolMode::Reflection, &reflect_memory).await;
    assert_eq!(reflect_memory.read("profile.md").unwrap(), "REWRITTEN");
    let guard = reflect_events.lock().unwrap();
    assert!(guard
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolResult { ok: true, .. })));
}

/// `web_fetch` is not a memory tool, so the reflection pass's
/// `ToolMode::Reflection` gate refuses it before dispatch — no network request
/// is ever made during consolidation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_fetch_is_refused_in_reflection_mode() {
    let (port, handle) = start_mock(
        vec![
            vec![
                tool_frag(0, "c1", "web_fetch", ""),
                // A loopback URL: if the reflection gate ever regressed, the
                // fetch would fail fast against the SSRF guard instead of
                // reaching example.com over the network.
                tool_frag(0, "", "", r#"{\"url\":\"http://127.0.0.1:1/\"}"#),
                finish_chunk("tool_calls"),
            ],
            vec![delta_chunk("done"), finish_chunk("stop")],
        ],
        0,
    );
    let cfg = AppConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "mock".into(),
        ..Default::default()
    };
    let mcp = tools::BraveSearch::default();
    let shell = crate::shell::ShellExecutor::new();
    let memory = temp_memory("webfetch-reflect");
    let approvals = tools::ApprovalRegistry::default();
    let flag = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = TestSink(events.clone());
    let messages = build_messages("p", None, &[], "hi", &[]).unwrap();
    let tools_list = tools::tool_specs(false);
    run_tool_loop(
        &cfg,
        "test-key",
        &mcp,
        &shell,
        &memory,
        &approvals,
        "conv_reflect",
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        ToolMode::Reflection,
    )
    .await
    .unwrap();
    let _ = handle.join();

    let guard = events.lock().unwrap();
    assert!(guard.iter().any(|e| matches!(
        e,
        StreamEvent::ToolResult { ok: false, output, .. }
            if output.contains("not available during memory consolidation")
    )));
}

/// In a live turn `web_fetch` is routed to `execute_web_tool`, not the host
/// fallback. A malformed URL exercises the routing with no network at all: the
/// host executor would answer `unknown tool: web_fetch`, so a `web_fetch:`
/// error proves the web-tool route was taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_fetch_in_live_mode_routes_to_web_tool() {
    let (port, handle) = start_mock(
        vec![
            vec![
                tool_frag(0, "c1", "web_fetch", ""),
                tool_frag(0, "", "", r#"{\"url\":\"ftp://example.com\"}"#),
                finish_chunk("tool_calls"),
            ],
            vec![delta_chunk("done"), finish_chunk("stop")],
        ],
        0,
    );
    let cfg = AppConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "mock".into(),
        ..Default::default()
    };
    let mcp = tools::BraveSearch::default();
    let shell = crate::shell::ShellExecutor::new();
    let memory = temp_memory("webfetch-live-route");
    let approvals = tools::ApprovalRegistry::default();
    let flag = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = TestSink(events.clone());
    let messages = build_messages("p", None, &[], "hi", &[]).unwrap();
    let tools_list = tools::tool_specs(false);
    run_tool_loop(
        &cfg,
        "test-key",
        &mcp,
        &shell,
        &memory,
        &approvals,
        "conv_live_route",
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        ToolMode::Live,
    )
    .await
    .unwrap();
    let _ = handle.join();

    let guard = events.lock().unwrap();
    assert!(
        guard.iter().any(|e| matches!(
            e,
            StreamEvent::ToolResult { ok: false, output, .. }
                if output.starts_with("web_fetch:")
                    && !output.contains("unknown tool: web_fetch")
        )),
        "web_fetch was not routed to the web tool"
    );
}

/// Extraction advertises no tools at all — it is pure summarization — so any
/// tool call the model emits anyway is refused rather than dispatched. That is
/// what makes the map phase safe to run concurrently: it cannot write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extract_mode_refuses_every_tool_call() {
    let (port, handle) = start_mock(
        vec![
            vec![
                tool_frag(0, "c1", "write_memory", ""),
                tool_frag(
                    0,
                    "",
                    "",
                    r#"{\"path\":\"profile.md\",\"content\":\"CLOBBERED\"}"#,
                ),
                finish_chunk("tool_calls"),
            ],
            vec![
                tool_frag(0, "c2", "save_memory", ""),
                tool_frag(
                    0,
                    "",
                    "",
                    r#"{\"path\":\"profile.md\",\"content\":\"APPENDED\"}"#,
                ),
                finish_chunk("tool_calls"),
            ],
            vec![
                tool_frag(0, "c3", "read_memory", ""),
                tool_frag(0, "", "", r#"{\"path\":\"profile.md\"}"#),
                finish_chunk("tool_calls"),
            ],
            vec![delta_chunk("[profile] likes tea"), finish_chunk("stop")],
        ],
        0,
    );
    let cfg = AppConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "mock".into(),
        ..Default::default()
    };
    let mcp = tools::BraveSearch::default();
    let shell = crate::shell::ShellExecutor::new();
    let memory = temp_memory("extract-mode");
    let approvals = tools::ApprovalRegistry::default();
    let flag = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = TestSink(events.clone());
    let messages = build_messages("p", None, &[], "hi", &[]).unwrap();
    let tools_list = tools::tool_specs(false);
    let result = run_tool_loop(
        &cfg,
        "test-key",
        &mcp,
        &shell,
        &memory,
        &approvals,
        "conv_extract",
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        ToolMode::Extract,
    )
    .await
    .unwrap();
    let _ = handle.join();

    // The summary is the pass's output; memory is untouched.
    assert_eq!(result.text, "[profile] likes tea");
    assert!(memory.read("profile.md").unwrap().is_empty());

    let guard = events.lock().unwrap();
    let refusals: Vec<&String> = guard
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolResult {
                ok: false, output, ..
            } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(
        refusals.len(),
        3,
        "every tool call must be refused during extraction"
    );
    assert!(refusals
        .iter()
        .all(|o| o.contains("not available during memory extraction")));
    assert!(!guard
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolResult { ok: true, .. })));
}

/// The extraction pass advertises no tools, so the request must omit the field
/// entirely — some providers reject an empty `tools` array.
#[test]
fn completion_body_omits_tools_when_none_are_advertised() {
    let messages = build_messages("p", None, &[], "hi", &[]).unwrap();
    let body = completion_body("m", &messages, &[], "", None).unwrap();
    assert!(body.get("tools").is_none());

    let advertised = tools::tool_specs(false);
    let body = completion_body("m", &messages, &advertised, "", None).unwrap();
    assert!(body.get("tools").is_some());
}

#[test]
fn build_history_messages_includes_memory_notes_as_system() {
    let row = crate::db::MessageRow {
        id: "m".into(),
        conversation_id: "c".into(),
        role: "memory".into(),
        index: 0,
        content: "Memory consolidated: profile.md".into(),
        model: None,
        provider: None,
        thinking_level: None,
        thinking: None,
        usage: None,
        stop_reason: None,
        attachments: None,
        created_at: 0,
    };
    let msgs = build_history_messages(&[row]).unwrap();
    let value = serde_json::to_value(&msgs).unwrap();
    assert_eq!(value[0]["role"], "system");
    assert_eq!(value[0]["content"], "Memory consolidated: profile.md");
}

#[test]
fn stream_events_serialize_camel_case_fields() {
    // The frontend reads `callId`/`stopReason`; enum-level `rename_all` only
    // renames variants, so field renaming is covered explicitly.
    let tool_call = serde_json::to_value(StreamEvent::ToolCall {
        call_id: "call_1".into(),
        name: "bash".into(),
        arguments: "{}".into(),
        gated: true,
    })
    .unwrap();
    assert_eq!(tool_call["type"], "toolCall");
    assert_eq!(tool_call["callId"], "call_1");
    assert_eq!(tool_call["gated"], true);

    let tool_result = serde_json::to_value(StreamEvent::ToolResult {
        call_id: "call_1".into(),
        name: "bash".into(),
        ok: true,
        output: "hi".into(),
    })
    .unwrap();
    assert_eq!(tool_result["callId"], "call_1");

    let done = serde_json::to_value(StreamEvent::Done {
        stop_reason: "stop".into(),
    })
    .unwrap();
    assert_eq!(done["stopReason"], "stop");
}

#[test]
fn sum_usage_normalizes_cached_tokens_across_shapes() {
    let mut total = json!({});
    // OpenAI-shape nested details.
    sum_usage(
        &mut total,
        &Some(json!({
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110,
            "prompt_tokens_details": { "cached_tokens": 40 }
        })),
    );
    assert_eq!(total["prompt_tokens"], 100);
    assert_eq!(total["cached_tokens"], 40);

    // Anthropic-shape flat key, summed with the previous round.
    sum_usage(
        &mut total,
        &Some(json!({ "prompt_tokens": 50, "cache_read_input_tokens": 5 })),
    );
    assert_eq!(total["prompt_tokens"], 150);
    assert_eq!(total["cached_tokens"], 45);
}

#[test]
fn build_system_prompt_orders_preferences_base_model_and_memory() {
    let memory = temp_memory("prompt");
    memory
        .write("preferences.md", "User prefers dark mode.")
        .unwrap();
    let prompt = build_system_prompt("BASE", "DeepSeek V4 Pro", "Talk like a pirate.", &memory, None);
    assert!(prompt.starts_with("Explicit user preferences"));
    assert!(prompt.contains("Talk like a pirate."));
    assert!(prompt.contains("BASE"));
    assert!(prompt.contains("running as the model \"DeepSeek V4 Pro\""));
    assert!(prompt.contains("Long-term memory"));
    assert!(prompt.contains("preferences.md"));
    assert!(prompt.contains("User prefers dark mode"));
    // Preferences come before the base prompt, and the model line after it.
    assert!(prompt.find("Talk like a pirate").unwrap() < prompt.find("BASE").unwrap());
    assert!(prompt.find("BASE").unwrap() < prompt.find("running as the model").unwrap());
}

#[test]
fn build_system_prompt_omits_empty_preferences() {
    let memory = temp_memory("prompt-empty");
    let prompt = build_system_prompt("BASE", "mock", "   ", &memory, None);
    assert!(prompt.starts_with("BASE"));
    assert!(!prompt.contains("Explicit user preferences (set directly"));
}

// ---------- live provider smoke test (opt-in) ----------

/// Live smoke test against a real OpenAI-compatible provider: run a tool-call
/// turn through the real request path, then replay the assistant tool-call
/// message with a synthetic `reasoning_content` echo and confirm the provider
/// accepts it (this is the DeepSeek-compat question; the trace is synthetic so
/// the result does not depend on whether the model emits one).
///
/// Ignored by default (network + tokens). Run explicitly:
///   cargo test --lib -- --ignored live_reasoning_echo
/// Override the target with `PI_LIVE_BASE_URL` / `PI_LIVE_MODEL`. The API key is
/// read from the OS keychain and never printed.
#[tokio::test]
#[ignore = "hits the real provider API; run explicitly with `--ignored`"]
async fn live_reasoning_echo_is_accepted_by_provider() {
    let base = std::env::var("PI_LIVE_BASE_URL")
        .unwrap_or_else(|_| "https://api.fireworks.ai/inference/v1".to_string());
    let model = std::env::var("PI_LIVE_MODEL")
        .unwrap_or_else(|_| "accounts/fireworks/models/deepseek-v4p1-flash".to_string());
    let Some(key) = crate::secrets::get().expect("keychain read") else {
        eprintln!("skipping: no API key in the OS keychain");
        return;
    };

    let tools = tools::tool_specs(false);
    let mut messages = build_messages(
        "You are a tool-using assistant. Think step by step, then call the requested tool.",
        None,
        &[],
        "Work out 47 * 89 step by step, then use the bash tool to verify it \
         (e.g. `echo $((47*89))`), and report both your computed answer and the tool output.",
        &[],
    )
    .unwrap();

    // Round 1: the model should request a tool and (for a reasoning model) emit a trace.
    let sink = TestSink(Arc::new(Mutex::new(Vec::new())));
    let body = completion_body(&model, &messages, &tools, "high", None).unwrap();
    let resp = open_completion_stream(&base, &key, &body)
        .await
        .expect("round 1 request failed");
    let round1 = run_completion(resp, &sink, &AtomicBool::new(false)).await;
    assert!(!round1.failed, "round 1 stream failed");

    if round1.tool_calls.is_empty() {
        eprintln!("inconclusive: the model did not request a tool; skipping");
        return;
    }
    eprintln!(
        "round 1: {} tool call(s); model reasoning {} (informational)",
        round1.tool_calls.len(),
        if round1.thinking.trim().is_empty() {
            "empty"
        } else {
            "present"
        }
    );

    // Replay the assistant tool-call message plus the tool result.
    let tool_calls = round1
        .tool_calls
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
        .collect::<Vec<_>>();
    messages.push(
        ChatCompletionRequestAssistantMessageArgs::default()
            .tool_calls(tool_calls)
            .build()
            .unwrap()
            .into(),
    );
    for c in &round1.tool_calls {
        messages.push(
            ChatCompletionRequestToolMessageArgs::default()
                .content("hi")
                .tool_call_id(c.id.clone())
                .build()
                .unwrap()
                .into(),
        );
    }

    // Round 2: echo a reasoning trace on the assistant tool-call message and
    // confirm the provider accepts it. The trace is synthetic so the wire
    // behaviour is deterministic even when the model emits no reasoning.
    let reasoning = "I should verify the arithmetic with the bash tool before answering.";
    let body2 = completion_body(&model, &messages, &tools, "high", Some(reasoning)).unwrap();
    assert!(
        body2["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.get("reasoning_content").is_some()),
        "expected reasoning_content in the round-2 request body"
    );
    let resp2 = open_completion_stream(&base, &key, &body2)
        .await
        .expect("round 2 rejected the echoed reasoning_content");
    let round2 = run_completion(resp2, &sink, &AtomicBool::new(false)).await;
    assert!(!round2.failed, "round 2 stream failed");
    assert!(
        !round2.text.trim().is_empty() || !round2.tool_calls.is_empty(),
        "round 2 produced neither text nor a tool call"
    );
    eprintln!("round 2 ok: {} chars", round2.text.len());
}

    // --- context compaction -------------------------------------------------

    fn row(id: &str, role: &str, content: &str) -> crate::db::MessageRow {
        crate::db::MessageRow {
            id: id.into(),
            conversation_id: "c".into(),
            role: role.into(),
            index: 0,
            content: content.into(),
            model: None,
            provider: None,
            thinking_level: None,
            thinking: None,
            usage: None,
            stop_reason: None,
            attachments: None,
            created_at: 0,
        }
    }

    fn conversation_with(
        summary: Option<&str>,
        through: Option<&str>,
    ) -> crate::db::Conversation {
        crate::db::Conversation {
            id: "c".into(),
            title: "t".into(),
            model: None,
            provider_id: None,
            system_prompt: None,
            compaction_summary: summary.map(str::to_string),
            compaction_through: through.map(str::to_string),
            created_at: 0,
            updated_at: 0,
        }
    }

    /// The summary is injected as a system message right after the system prompt,
    /// ahead of the verbatim history — the exact layout compaction promises.
    #[test]
    fn context_puts_summary_before_history() {
        let history = [row("m1", "user", "hello")];
        let msgs = build_context("sys", Some("THE-SUMMARY"), &history).unwrap();
        let v = serde_json::to_value(&msgs).unwrap();
        let roles: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["system", "system", "user"]);
        assert_eq!(v[1]["content"], "THE-SUMMARY");
    }

    #[test]
    fn compacted_history_slices_after_the_boundary_id() {
        let history = vec![
            row("a", "user", "one"),
            row("b", "assistant", "two"),
            row("c", "user", "three"),
        ];
        let conv = conversation_with(Some("SUM"), Some("b"));
        let (summary, start) = compacted_history(&conv, &history);
        assert_eq!(summary.as_deref(), Some("SUM"));
        assert_eq!(start, 2);
        assert_eq!(history[start].id, "c");
    }

    /// A summary without its (local-only) boundary — e.g. synced from another
    /// device — must be ignored, not applied at a guessed split.
    #[test]
    fn compacted_history_ignores_a_missing_boundary() {
        let history = vec![row("a", "user", "one")];
        let conv = conversation_with(Some("SUM"), Some("gone"));
        let (summary, start) = compacted_history(&conv, &history);
        assert!(summary.is_none());
        assert_eq!(start, 0);
    }

    /// The inverse of the above, and the one that bites in practice: a boundary
    /// whose summary was nulled by a sync pull's last-writer-wins (the summary is
    /// on the synced row; the boundary is local-only) must NOT slice history away
    /// with nothing injected in its place. It falls back to the full history.
    #[test]
    fn compacted_history_ignores_a_boundary_without_a_summary() {
        let history = vec![row("a", "user", "one"), row("b", "user", "two")];
        for summary in [None, Some(""), Some("   ")] {
            let conv = conversation_with(summary, Some("a"));
            let (s, start) = compacted_history(&conv, &history);
            assert!(s.is_none(), "summary {summary:?} must not be applied");
            assert_eq!(start, 0, "no messages may be dropped");
        }
    }

    #[test]
    fn compacted_history_handles_edge_boundaries() {
        let history = vec![row("a", "user", "one"), row("b", "user", "two")];

        // Boundary at the first message: the whole history is folded.
        let (s, start) = compacted_history(&conversation_with(Some("SUM"), Some("a")), &history);
        assert_eq!(s.as_deref(), Some("SUM"));
        assert_eq!(start, 1);

        // Boundary at the last message: nothing is kept verbatim.
        let (s, start) = compacted_history(&conversation_with(Some("SUM"), Some("b")), &history);
        assert_eq!(s.as_deref(), Some("SUM"));
        assert_eq!(start, 2);
        assert!(history[start..].is_empty());
    }

    /// A history that fits entirely within the verbatim budget must not fold
    /// anything — even (defensively) when it begins on a `tool` row, `start` must
    /// stay 0 rather than advancing past it.
    #[test]
    fn keep_from_leaves_a_fully_within_budget_history_alone() {
        let history = vec![row("a", "tool", "result"), row("b", "user", "hi")];
        assert_eq!(keep_from(&history, u64::MAX), 0);
    }

    /// The kept tail must never begin with a `tool` row: its assistant tool-call
    /// would have been folded into the summary, orphaning the result.
    #[test]
    fn keep_from_never_starts_on_a_tool_row() {
        let history = vec![
            row("a", "user", "hi"),
            row("b", "assistant", "{\"tool_calls\":[]}"),
            row("c", "tool", "result"),
            row("d", "user", "next"),
        ];
        let start = keep_from(&history, 10);
        assert_eq!(start, 3, "the tool result must be folded, not lead the tail");
        assert_ne!(history[start].role, "tool");
    }

    /// Past the threshold a turn summarizes the older history and sends the
    /// summary plus only the recent verbatim tail; a follow-up turn reuses the
    /// summary without re-compacting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compaction_folds_old_history_then_reuses_the_summary() {
        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    delta_chunk("SUMMARY-OF-OLDER"),
                    finish_chunk("stop"),
                    usage_chunk(),
                ],
                vec![delta_chunk("first"), finish_chunk("stop"), usage_chunk()],
                vec![delta_chunk("second"), finish_chunk("stop"), usage_chunk()],
            ],
            0,
        );
        let db = Arc::new(open_db("compact"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);

        // Window 30k → threshold 24k; 60 × ~500-token messages ≈ 30k tokens, so
        // turn 1 is over threshold while the fixed 15k verbatim budget still
        // leaves ~15k to fold.
        let filler = "x".repeat(2000);
        for i in 0..60 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            let content = if i == 0 {
                format!("ANCIENT-MARKER {filler}")
            } else {
                format!("OLD-{i} {filler}")
            };
            crate::db::insert_message(
                &db,
                cid.clone(),
                role.into(),
                content,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }

        let mut cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        cfg.model_overrides.insert(
            "mock".into(),
            crate::config::ModelOverride {
                context_window: Some(30_000),
                ..Default::default()
            },
        );

        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let memory = temp_memory("compact");
        for turn in 0..2 {
            let flag = Arc::new(AtomicBool::new(false));
            run_chat_turn(
                &db,
                &cfg,
                "test-key",
                &brave,
                &shell,
                &memory,
                &approvals,
                flag,
                &sink,
                cid.clone(),
                format!("turn {turn}"),
                Vec::new(),
            false,
            )
            .await
            .unwrap();
        }
        let _ = handle.join();

        let caps = bodies.lock().unwrap().clone();
        assert_eq!(caps.len(), 3, "expected summarize + two live turns");

        // The first upstream call is the summarizer: it sees the folded history
        // and advertises no tools (pure summarization).
        assert!(
            caps[0].contains("ANCIENT-MARKER"),
            "the summarizer must see the folded history"
        );
        assert!(
            request_body(&caps[0]).get("tools").is_none(),
            "the summarizer must advertise no tools"
        );

        // Both live turns carry the summary and no longer the folded-away content.
        for live in [&caps[1], &caps[2]] {
            assert!(
                live.contains("SUMMARY-OF-OLDER"),
                "the live turn must carry the summary"
            );
            assert!(
                !live.contains("ANCIENT-MARKER"),
                "folded content must be gone from the live turn"
            );
        }

        let conv = crate::db::get_conversation(&db, &cid).unwrap().unwrap();
        assert_eq!(conv.compaction_summary.as_deref(), Some("SUMMARY-OF-OLDER"));
        assert!(conv.compaction_through.is_some());
    }

    // --- YOLO auto-approve --------------------------------------------------

    /// With `auto_approve_tools` on, a gated call runs immediately, is announced
    /// as ungated (so the UI never shows the approve/deny prompt), and the loop
    /// never parks on the approval channel.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn yolo_auto_approves_gated_tool_without_prompting() {
        let (port, handle) = start_mock(
            vec![
                vec![
                    tool_frag(0, "call_1", "bash", ""),
                    tool_frag(0, "", "", r#"{\"command\":\"echo yolo\"}"#),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("done"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("yolo"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            auto_approve_tools: true,
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("yolo");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &brave, &shell, &memory, &approvals2, flag, &sink,
                cid2, "go".into(), Vec::new(),
            false,
            )
            .await
        });
        // Deliberately no `approvals.resolve`: YOLO must finish on its own. A
        // hang here is a real failure (the loop parked waiting for approval).
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("YOLO turn must not wait for an approval")
            .unwrap()
            .unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 4);
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], false);
        assert_eq!(tr["output"], "yolo");

        let guard = events.lock().unwrap();
        assert!(
            guard
                .iter()
                .any(|e| matches!(e, StreamEvent::ToolCall { gated: false, name, .. } if name == "bash")),
            "a YOLO gated call must be emitted ungated"
        );
        assert!(
            !guard.iter().any(|e| matches!(e, StreamEvent::ToolCall { gated: true, .. })),
            "no gated tool call may be emitted under YOLO"
        );
    }

    // --- editable messages (edit -> truncate) -------------------------------

    /// Insert four alternating turns and return their ids in order.
    fn seed_turns(db: &Db, cid: &str) -> Vec<String> {
        let mut ids = Vec::new();
        for (role, content) in [
            ("user", "one"),
            ("assistant", "two"),
            ("user", "three"),
            ("assistant", "four"),
        ] {
            let row = crate::db::insert_message(
                db,
                cid.to_string(),
                role.into(),
                content.into(),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            ids.push(row.id);
        }
        ids
    }

    /// Editing an early user message forks the conversation: later messages are
    /// deleted, the compaction summary that folded the removed tail is cleared,
    /// and the reflection watermark drops below the edit so it re-runs.
    #[test]
    fn edit_user_message_truncates_and_clears_stale_maintenance_state() {
        use crate::attachments::Attachment;
        let db = open_db("edit-truncate");
        let cid = insert_conv(&db);
        let ids = seed_turns(&db, &cid);
        // Compaction folded through the last message; reflection covered all of it.
        crate::db::set_compaction(&db, &cid, "SUM", &ids[3]).unwrap();
        crate::db::set_last_reflected_index(&db, &cid, 3).unwrap();

        let att = Attachment {
            id: "a".into(),
            name: "x.txt".into(),
            mime: "text/plain".into(),
            size: 1,
            kind: "file".into(),
            data_url: None,
            text: Some("new context".into()),
        };
        crate::db::apply_user_message_edit(&db, &cid, &ids[0], "edited", Some(vec![att])).unwrap();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 1, "everything after the edited turn is gone");
        assert_eq!(rows[0].id, ids[0]);
        assert_eq!(rows[0].content, "edited");
        assert!(rows[0]
            .attachments
            .as_deref()
            .unwrap()
            .contains("new context"));

        let conv = crate::db::get_conversation(&db, &cid).unwrap().unwrap();
        assert!(
            conv.compaction_summary.is_none(),
            "a summary folding a deleted message must be cleared"
        );
        assert!(conv.compaction_through.is_none());
        assert_eq!(crate::db::last_reflected_index(&db, &cid).unwrap(), Some(-1));
    }

    /// When the compaction boundary sits strictly before the edit it survives the
    /// truncation, so the summary is kept; earlier messages are untouched.
    #[test]
    fn edit_user_message_keeps_earlier_messages_and_a_surviving_boundary() {
        let db = open_db("edit-keep");
        let cid = insert_conv(&db);
        let ids = seed_turns(&db, &cid);
        crate::db::set_compaction(&db, &cid, "SUM", &ids[1]).unwrap();
        crate::db::set_last_reflected_index(&db, &cid, 3).unwrap();

        crate::db::apply_user_message_edit(&db, &cid, &ids[2], "edited", None).unwrap();

        let rows = msgs(&db, &cid);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].content, "edited");
        let conv = crate::db::get_conversation(&db, &cid).unwrap().unwrap();
        assert_eq!(conv.compaction_summary.as_deref(), Some("SUM"));
        assert_eq!(conv.compaction_through.as_deref(), Some(ids[1].as_str()));
        assert_eq!(crate::db::last_reflected_index(&db, &cid).unwrap(), Some(1));
    }

    #[test]
    fn edit_user_message_rejects_non_user_or_missing_targets() {
        let db = open_db("edit-reject");
        let cid = insert_conv(&db);
        let row = crate::db::insert_message(
            &db,
            cid.clone(),
            "assistant".into(),
            "two".into(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(crate::db::apply_user_message_edit(&db, &cid, &row.id, "x", None).is_err());
        assert!(crate::db::apply_user_message_edit(&db, &cid, "missing", "x", None).is_err());
        assert_eq!(msgs(&db, &cid)[0].content, "two");
    }

    /// Editing a user message keeps that row and re-runs the turn with
    /// `regenerate = true`: the backend must neither persist a second `user` row
    /// nor send the turn twice in the request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn regenerate_turn_does_not_duplicate_the_edited_user_row() {
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![
                delta_chunk("Edited reply"),
                finish_chunk("stop"),
                usage_chunk(),
            ]],
            0,
        );
        let db = Arc::new(open_db("regen-edit"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);

        // Seed the user turn, then edit it through the real DB path (which keeps
        // the row and clears everything after it).
        let row = crate::db::insert_message(
            &db,
            cid.clone(),
            "user".into(),
            "original".into(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        crate::db::apply_user_message_edit(&db, &cid, &row.id, "edited text", None).unwrap();

        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let memory = temp_memory("regen-edit");
        let flag = Arc::new(AtomicBool::new(false));

        run_chat_turn(
            &db,
            &cfg,
            "test-key",
            &brave,
            &shell,
            &memory,
            &approvals,
            flag,
            &sink,
            cid.clone(),
            "edited text".into(),
            Vec::new(),
            true,
        )
        .await
        .unwrap();
        let _ = handle.join();

        // Exactly one user row, and it is the pre-existing edited row.
        let rows = msgs(&db, &cid);
        let user_rows: Vec<_> = rows.iter().filter(|m| m.role == "user").collect();
        assert_eq!(user_rows.len(), 1, "regenerate must not insert a second user row");
        assert_eq!(user_rows[0].id, row.id);
        assert_eq!(user_rows[0].content, "edited text");
        assert_eq!(rows.len(), 2, "the edited user turn + the assistant reply");
        assert_eq!(rows[1].role, "assistant");
        assert_eq!(rows[1].content, "Edited reply");

        // And the upstream request carries the turn exactly once.
        let caps = bodies.lock().unwrap().clone();
        let req = request_body(&caps[0]);
        let sent: Vec<_> = req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "user")
            .collect();
        assert_eq!(sent.len(), 1, "the user turn must appear once upstream");
        assert_eq!(sent[0]["content"], "edited text");
    }

    /// The `regenerate` dedup must not rely on the edited row being the literal
    /// tail: a row appended after the edit (e.g. a `memory` note) must still
    /// leave the turn sent exactly once upstream.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn regenerate_dedup_survives_a_row_appended_after_the_edit() {
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![
                delta_chunk("Reply"),
                finish_chunk("stop"),
                usage_chunk(),
            ]],
            0,
        );
        let db = Arc::new(open_db("regen-appended"));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);

        let row = crate::db::insert_message(
            &db,
            cid.clone(),
            "user".into(),
            "original".into(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        crate::db::apply_user_message_edit(&db, &cid, &row.id, "edited text", None).unwrap();
        // A row lands after the edit, so the edited user row is no longer the
        // last message — the content match (not a blind tail pop) must find it.
        crate::db::insert_message(
            &db,
            cid.clone(),
            "memory".into(),
            "a note".into(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let memory = temp_memory("regen-appended");
        let flag = Arc::new(AtomicBool::new(false));

        run_chat_turn(
            &db,
            &cfg,
            "test-key",
            &brave,
            &shell,
            &memory,
            &approvals,
            flag,
            &sink,
            cid.clone(),
            "edited text".into(),
            Vec::new(),
            true,
        )
        .await
        .unwrap();
        let _ = handle.join();

        // The request still carries the user turn exactly once.
        let caps = bodies.lock().unwrap().clone();
        let req = request_body(&caps[0]);
        let sent: Vec<_> = req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "user")
            .collect();
        assert_eq!(sent.len(), 1, "the edited turn must still appear once upstream");
        assert_eq!(sent[0]["content"], "edited text");

        // No second user row was persisted, and the appended row survived.
        let rows = msgs(&db, &cid);
        assert_eq!(rows.iter().filter(|m| m.role == "user").count(), 1);
        assert!(rows.iter().any(|m| m.role == "memory"));
    }

    // --- read_file images (item 9) ------------------------------------------

    /// Register `mock` as a vision model for the given provider key so the chat
    /// loop's `meta_for_provider` lookup reports it multimodal.
    fn mark_model_vision(db: &Db, provider_key: &str) {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO model_prices (provider, model_id, modalities) VALUES (?1, 'mock', ?2)",
            rusqlite::params![provider_key, r#"{"input":["text","image"]}"#],
        )
        .unwrap();
    }

    fn escape_args(v: Value) -> String {
        v.to_string().replace('"', "\\\"")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_file_image_on_multimodal_model_injects_an_image_part() {
        let img =
            std::env::temp_dir().join(format!("pi-read-image-vision-{}.png", std::process::id()));
        std::fs::write(&img, [0u8, 1, 2, 3]).unwrap();
        let path = img.to_str().unwrap().to_string();
        let args = escape_args(json!({ "path": path }));

        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    tool_frag(0, "call_1", "read_file", ""),
                    tool_frag(0, "", "", &args),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("I see it"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("read-image-vision"));
        let base_url = format!("http://127.0.0.1:{port}/v1");
        mark_model_vision(&db, &base_url);

        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: base_url.clone(),
            model: "mock".into(),
            auto_approve_tools: true,
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));
        run_chat_turn(
            &db, &cfg, "test-key", &brave, &shell, &temp_memory("read-image-vision"), &approvals,
            flag, &sink, cid.clone(), "look".into(), Vec::new(),
        false,
        )
        .await
        .unwrap();
        let _ = handle.join();

        // The tool result is a short text note, not the image bytes.
        let rows = msgs(&db, &cid);
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], false);
        assert!(
            tr["output"].as_str().unwrap().contains("image"),
            "expected an image note, got {}",
            tr["output"]
        );
        assert_eq!(rows.len(), 4, "the synthetic image turn is not persisted");

        // The follow-up request carries the image as a user image_url part.
        let caps = bodies.lock().unwrap();
        let second = request_body(&caps[1]);
        let msgs_v = second["messages"].as_array().unwrap();
        assert!(
            msgs_v.iter().any(|m| {
                m["role"] == "user"
                    && m["content"]
                        .as_array()
                        .map(|parts| {
                            parts.iter().any(|p| p["type"] == "image_url")
                        })
                        .unwrap_or(false)
            }),
            "expected a synthetic user image_url message: {second}"
        );
        assert!(caps[1].contains("AAECAw=="), "image bytes must reach the model");
        let _ = std::fs::remove_file(&img);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_file_image_on_text_model_reports_it_cannot_view() {
        let img =
            std::env::temp_dir().join(format!("pi-read-image-text-{}.png", std::process::id()));
        std::fs::write(&img, [0u8, 1, 2, 3]).unwrap();
        let path = img.to_str().unwrap().to_string();
        let args = escape_args(json!({ "path": path }));

        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    tool_frag(0, "call_1", "read_file", ""),
                    tool_frag(0, "", "", &args),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("ok"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("read-image-text"));
        // No model_prices row: the provider is unknown, so vision is false.
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            auto_approve_tools: true,
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));
        run_chat_turn(
            &db, &cfg, "test-key", &brave, &shell, &temp_memory("read-image-text"), &approvals,
            flag, &sink, cid.clone(), "look".into(), Vec::new(),
        false,
        )
        .await
        .unwrap();
        let _ = handle.join();

        let rows = msgs(&db, &cid);
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], false);
        assert!(
            tr["output"].as_str().unwrap().contains("cannot view"),
            "expected a cannot-view note, got {}",
            tr["output"]
        );

        // No image part is sent to a text-only model.
        assert!(!bodies.lock().unwrap()[1].contains("image_url"));
        let _ = std::fs::remove_file(&img);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_file_oversized_image_is_not_injected() {
        // Sparse file: metadata reports the length without writing the bytes.
        let img = std::env::temp_dir().join(format!(
            "pi-read-image-oversize-{}.png",
            std::process::id()
        ));
        let f = std::fs::File::create(&img).unwrap();
        f.set_len(crate::attachments::MAX_IMAGE_BYTES + 1).unwrap();
        drop(f);
        let path = img.to_str().unwrap().to_string();
        let args = escape_args(json!({ "path": path }));

        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    tool_frag(0, "call_1", "read_file", ""),
                    tool_frag(0, "", "", &args),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("ok"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("read-image-oversize"));
        let base_url = format!("http://127.0.0.1:{port}/v1");
        // Even a vision model must not receive an oversized image.
        mark_model_vision(&db, &base_url);

        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: base_url.clone(),
            model: "mock".into(),
            auto_approve_tools: true,
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));
        run_chat_turn(
            &db, &cfg, "test-key", &brave, &shell, &temp_memory("read-image-oversize"),
            &approvals, flag, &sink, cid.clone(), "look".into(), Vec::new(),
        false,
        )
        .await
        .unwrap();
        let _ = handle.join();

        // A plain text result telling the model it cannot view the image.
        let rows = msgs(&db, &cid);
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], false);
        assert!(
            tr["output"].as_str().unwrap().contains("too large"),
            "expected a too-large note, got {}",
            tr["output"]
        );
        assert_eq!(rows.len(), 4, "no synthetic image turn is persisted");

        // The image is never injected as a data URL.
        let caps = bodies.lock().unwrap();
        assert!(!caps[1].contains("image_url"), "oversized image must not be sent");
        assert!(!caps[1].contains("base64"), "oversized image must not be read");
        let _ = std::fs::remove_file(&img);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_file_svg_falls_back_to_text_and_is_not_an_image_part() {
        let svg = std::env::temp_dir().join(format!("pi-read-svg-{}.svg", std::process::id()));
        std::fs::write(&svg, "<svg><rect/></svg>").unwrap();
        let path = svg.to_str().unwrap().to_string();
        let args = escape_args(json!({ "path": path }));

        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![
                    tool_frag(0, "call_1", "read_file", ""),
                    tool_frag(0, "", "", &args),
                    finish_chunk("tool_calls"),
                ],
                vec![delta_chunk("ok"), finish_chunk("stop")],
            ],
            0,
        );
        let db = Arc::new(open_db("read-svg"));
        let base_url = format!("http://127.0.0.1:{port}/v1");
        // Even with a vision model, an SVG must not be turned into an image part.
        mark_model_vision(&db, &base_url);

        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: base_url.clone(),
            model: "mock".into(),
            auto_approve_tools: true,
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let flag = Arc::new(AtomicBool::new(false));
        run_chat_turn(
            &db, &cfg, "test-key", &brave, &shell, &temp_memory("read-svg"), &approvals, flag,
            &sink, cid.clone(), "look".into(), Vec::new(),
        false,
        )
        .await
        .unwrap();
        let _ = handle.join();

        // The SVG is read as ordinary text, not as a base64 image.
        let rows = msgs(&db, &cid);
        let tr: Value = serde_json::from_str(&rows[2].content).unwrap();
        assert_eq!(tr["error"], false);
        assert!(
            tr["output"].as_str().unwrap().contains("<svg>"),
            "expected the SVG text as a tool result, got {}",
            tr["output"]
        );
        assert!(!bodies.lock().unwrap()[1].contains("image_url"));
        let _ = std::fs::remove_file(&svg);
    }

    #[test]
    fn build_system_prompt_includes_device_sentence_and_start_date() {
        let memory = temp_memory("prompt-device-date");
        let prompt_no_date = build_system_prompt("BASE", "mock", "", &memory, None);
        assert!(
            prompt_no_date.contains("own device"),
            "system prompt must state tools run on user's own device"
        );
        assert!(
            prompt_no_date.contains("not on a remote server")
                || prompt_no_date.contains("not on a server"),
            "device sentence must mention not on a server"
        );
        // With a start date.
        let ms = 1_789_646_269_072; // 2026-09-17
        let prompt_with_date =
            build_system_prompt("BASE", "mock", "", &memory, Some(ms));
        assert!(
            prompt_with_date.contains("This conversation started on 2026-09-17 (UTC)."),
            "start date line missing or wrong: {prompt_with_date}"
        );
        // Ensure ordering still holds: base before model, device after model, memory last.
        assert!(prompt_with_date.find("BASE").unwrap() < prompt_with_date.find("running as the model").unwrap());
        assert!(prompt_with_date.find("running as the model").unwrap() < prompt_with_date.find("own device").unwrap());
        assert!(prompt_with_date.find("own device").unwrap() < prompt_with_date.find("Long-term memory").unwrap());
    }

    #[test]
    fn format_date_produces_expected_utc_date() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_789_646_269_072), "2026-09-17");
        // Test the exact start-date formatting used in the system prompt.
        assert_eq!(format_date(1_760_000_000_000), "2025-10-09");
    }

    #[test]
    fn estimate_context_tokens_ignores_base64_payloads() {
        use crate::attachments::Attachment;
        let plain = estimate_context_tokens(&[user_request_message("hi", &[]).unwrap()]);
        let image = Attachment {
            id: "a".into(),
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 600_000,
            kind: "image".into(),
            data_url: Some(format!("data:image/png;base64,{}", "A".repeat(600_000))),
            ..Default::default()
        };
        let with_image =
            estimate_context_tokens(&[user_request_message("hi", &[image]).unwrap()]);
        // A 600 kB base64 payload is ~150k characters. Charging that as text (the
        // old behaviour) is what compacted conversations merely *containing* a
        // pasted screenshot — history replays every image on every turn. It must
        // count as roughly one image instead.
        assert!(
            with_image < plain + 10_000,
            "base64 payload leaked into the estimate: plain={plain} with_image={with_image}"
        );
        assert!(with_image >= plain + IMAGE_TOKENS - 50);
        // The trigger adds only the not-yet-sent turn to the reported count.
        assert_eq!(
            estimate_turn_tokens("hi", &[]),
            "hi".len() as u64 / CHARS_PER_TOKEN + 8
        );
    }

    #[test]
    fn should_add_time_note_triggers_correctly() {
        // No prior user => first message needs note.
        assert!(should_add_time_note(&[], 1_000_000));
        // `MessageRow` is not `Clone`, so build a fresh row per case.
        let mk = |created_at: i64| crate::db::MessageRow {
            id: "u1".into(),
            conversation_id: "c".into(),
            role: "user".into(),
            index: 0,
            content: "hi".into(),
            model: None,
            provider: None,
            thinking_level: None,
            thinking: None,
            usage: None,
            stop_reason: None,
            attachments: None,
            created_at,
        };
        // Recent user (2 min ago) => no note.
        assert!(!should_add_time_note(&[mk(1_000_000)], 1_000_000 + 2 * 60 * 1000));
        // Exactly 5 min => trigger.
        assert!(should_add_time_note(&[mk(1_000_000)], 1_000_000 + 5 * 60 * 1000));
        // >5 min => trigger.
        assert!(should_add_time_note(&[mk(1_000_000)], 1_000_000 + 6 * 60 * 1000));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_base64_with_low_reported_context_does_not_compact() {
        // Window 30k => threshold 24k. Estimate with large base64 ~28k > threshold,
        // reported 5k < threshold => new logic must NOT compact.
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![delta_chunk("ok"), finish_chunk("stop"), usage_chunk()]],
            0,
        );
        let db = Arc::new(open_db("large-low"));
        let cid = insert_conv(&db);
        // Build history: 40 filler messages (2000 chars each) => ~20k tokens
        // plus one large image attachment to push estimate over threshold.
        for i in 0..40 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            let content = "x".repeat(2000);
            if role == "assistant" {
                // Only the last assistant matters for reported tokens; set low.
                let usage = if i == 39 {
                    Some(r#"{"prompt_tokens":1000,"completion_tokens":1000,"context_tokens":5000}"#.to_string())
                } else {
                    None
                };
                crate::db::insert_message(&db, cid.clone(), role.into(), content, None, None, None, None, usage, None).unwrap();
            } else if i == 38 {
                // Second-last user carries the large image.
                let large_b64 = "A".repeat(50000);
                let att = crate::attachments::Attachment {
                    id: "a1".into(),
                    name: "pic.png".into(),
                    mime: "image/png".into(),
                    size: 50000,
                    kind: "image".into(),
                    data_url: Some(format!("data:image/png;base64,{large_b64}")),
                    text: None,
                };
                let att_json = serde_json::to_string(&vec![att]).unwrap();
                crate::db::insert_message_full(&db, cid.clone(), role.into(), content, None, None, None, None, None, None, Some(att_json)).unwrap();
            } else {
                crate::db::insert_message(&db, cid.clone(), role.into(), content, None, None, None, None, None, None).unwrap();
            }
        }
        let mut cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        cfg.model_overrides.insert(
            "mock".into(),
            crate::config::ModelOverride {
                context_window: Some(30_000),
                ..Default::default()
            },
        );
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let memory = temp_memory("large-low");
        let flag = Arc::new(AtomicBool::new(false));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        run_chat_turn(&db, &cfg, "test-key", &brave, &shell, &memory, &approvals, flag, &sink, cid.clone(), "hi".into(), Vec::new(), false).await.unwrap();
        let _ = handle.join();
        let caps = bodies.lock().unwrap().clone();
        // Only the live turn, no summarizer. With old .max() logic this would be 2 (summarizer + live).
        assert_eq!(caps.len(), 1, "large base64 but low reported context must NOT trigger compaction; got {} requests", caps.len());
        assert!(caps[0].contains("hi"));
        assert!(!caps[0].contains("SUMMARY-OF-OLDER"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reported_context_above_threshold_does_compact() {
        // Window 30k => threshold 24k. History estimate ~15k < threshold, reported 25k > threshold => must compact.
        let (port, bodies, handle) = start_mock_capture(
            vec![
                vec![delta_chunk("SUMMARY-OF-OLDER"), finish_chunk("stop"), usage_chunk()],
                vec![delta_chunk("ok"), finish_chunk("stop"), usage_chunk()],
            ],
            0,
        );
        let db = Arc::new(open_db("reported-high"));
        let cid = insert_conv(&db);
        for i in 0..60 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            let content = "x".repeat(1000);
            if role == "assistant" {
                let usage = if i == 59 {
                    Some(r#"{"prompt_tokens":1000,"completion_tokens":1000,"context_tokens":25000}"#.to_string())
                } else {
                    None
                };
                crate::db::insert_message(&db, cid.clone(), role.into(), content, None, None, None, None, usage, None).unwrap();
            } else {
                crate::db::insert_message(&db, cid.clone(), role.into(), content, None, None, None, None, None, None).unwrap();
            }
        }
        let mut cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        cfg.model_overrides.insert(
            "mock".into(),
            crate::config::ModelOverride {
                context_window: Some(30_000),
                ..Default::default()
            },
        );
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let memory = temp_memory("reported-high");
        let flag = Arc::new(AtomicBool::new(false));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        run_chat_turn(&db, &cfg, "test-key", &brave, &shell, &memory, &approvals, flag, &sink, cid.clone(), "hi".into(), Vec::new(), false).await.unwrap();
        let _ = handle.join();
        let caps = bodies.lock().unwrap().clone();
        assert_eq!(caps.len(), 2, "reported context above threshold must trigger compaction; got {:?}", caps.len());
        assert!(caps[0].contains("ANCIENT") || caps[0].contains(&"x".repeat(10)) || caps[0].contains("SUMMARY") || request_body(&caps[0]).to_string().contains("Fold the exchange"), "first request should be summarizer");
        assert!(caps[1].contains("SUMMARY-OF-OLDER"), "live turn must carry summary");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn time_note_appears_for_first_message() {
        let (port, bodies, handle) = start_mock_capture(
            vec![vec![delta_chunk("hi"), finish_chunk("stop")]],
            0,
        );
        let db = Arc::new(open_db("time-first"));
        let cid = insert_conv(&db);
        let cfg = AppConfig {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "mock".into(),
            ..Default::default()
        };
        let brave = tools::BraveSearch::default();
        let shell = crate::shell::ShellExecutor::new();
        let memory = temp_memory("time-first");
        let flag = Arc::new(AtomicBool::new(false));
        let approvals = Arc::new(tools::ApprovalRegistry::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink(events.clone());
        // Empty history => first message must have time note.
        run_chat_turn(&db, &cfg, "test-key", &brave, &shell, &memory, &approvals, flag, &sink, cid.clone(), "first".into(), Vec::new(), false).await.unwrap();
        let _ = handle.join();
        let caps = bodies.lock().unwrap().clone();
        assert_eq!(caps.len(), 1);
        assert!(
            caps[0].contains("Current time:"),
            "first message must include time note, got: {}", caps[0]
        );
        assert!(caps[0].contains("UTC."), "time note must end with UTC.");
    }

    #[test]
    fn time_note_absent_from_reflection_build() {
        let memory = temp_memory("reflection-time-note");
        let prompt = build_system_prompt("BASE", "mock", "", &memory, Some(0));
        assert!(!prompt.contains("Current time:"), "system prompt must not contain time note");
        // Reflection builds context directly without time note.
        let history = vec![row("a", "user", "hello")];
        let msgs = build_context(&prompt, None, &history).unwrap();
        let serialized = serde_json::to_string(&msgs).unwrap();
        assert!(!serialized.contains("Current time:"), "reflection build must not contain time note");
        // Also verify the time note helper formats correctly.
        let note = format_date_time(0);
        assert_eq!(note, "1970-01-01 00:00");
        let note_ms = 1_789_646_269_072; // 2026-09-17 11:57 UTC
        assert!(format_date_time(note_ms).starts_with("2026-09-17 11:57"));
    }
