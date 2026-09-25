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
        Db(std::sync::Mutex::new(conn))
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
            &build_messages("p", &[], "hi", &[]).unwrap(),
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
            &build_messages("p", &[], "hi", &[]).unwrap(),
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
            &build_messages("p", &[], "hi", &[]).unwrap(),
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
            &build_messages("p", &[], "hi", &[]).unwrap(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("single");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid_task, "hi".into(), Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("prompt-request");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid_task, "hi".into(),
                Vec::new(),
            )
            .await
        });
        task.await.unwrap().unwrap();
        let _ = handle.join();

        let request = bodies.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("User preferences:"));
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("cost");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid_task, "hi".into(),
                Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2) = (db.clone(), approvals.clone());
        let cid_task = cid.clone();
        let memory = temp_memory("reasoning");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid_task, "q".into(), Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("echo-reasoning");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid2, "run echo hi".into(), Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();

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
        let mcp2 = tools::McpClient::new();
        let shell2 = crate::shell::ShellRegistry::new();
        run_chat_turn(
            &db, &cfg, "test-key", &mcp2, &shell2, &memory2, &approvals, flag2, &sink2,
            cid.clone(), "again".into(), Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("echo-off");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink,
                cid2, "run echo hi".into(), Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let cid2 = cid.clone();
        let memory = temp_memory("deny");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag, &sink, cid2, "do it".into(), Vec::new(),
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
        let mcp = tools::McpClient::new();
        let shell = crate::shell::ShellRegistry::new();
        let flag = Arc::new(AtomicBool::new(false));

        let (db2, approvals2, events2) = (db.clone(), approvals.clone(), events.clone());
        let flag2 = flag.clone();
        let cid2 = cid.clone();
        let memory = temp_memory("abort");
        let task = tokio::spawn(async move {
            run_chat_turn(
                &db2, &cfg, "test-key", &mcp, &shell, &memory, &approvals2, flag2, &sink, cid2, "say something".into(), Vec::new(),
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
        .save_entry("User prefers dark mode.", None, Some("preference"), 3, "conv_p")
        .unwrap();
    let prompt = build_system_prompt("BASE", "DeepSeek V4 Pro", "Talk like a pirate.", &memory);
    assert!(prompt.starts_with("User preferences:\nTalk like a pirate."));
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
    let prompt = build_system_prompt("BASE", "mock", "   ", &memory);
    assert!(prompt.starts_with("BASE"));
    assert!(!prompt.contains("User preferences"));
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
