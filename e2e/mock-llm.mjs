// Minimal OpenAI-compatible mock used by the GUI e2e harness.
//
// Serves GET /v1/models and streams a scripted chat completion over SSE:
//   - reasoning deltas (reasoning_content) so the "Thought" trace populates
//   - content deltas split across several events
//   - a usage-only chunk
//   - a final content chunk (" FINALTAIL") that deliberately omits the trailing
//     blank line, exercising the residual-SSE flush in run_completion.
import http from "node:http";

const PORT = Number(process.env.MOCK_PORT ?? 8317);
const THINK_GAP = Number(process.env.MOCK_THINK_GAP ?? 700);
const TEXT_GAP = Number(process.env.MOCK_TEXT_GAP ?? 600);

const THINKING = [
  "Let me think. ",
  "The user just greeted me, so a short friendly reply is right.",
];
const TEXT = [
  "Hello! This is a mocked reply. ",
  "It should arrive complete, including this final sentence.",
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function chunk(delta, extra = {}) {
  return {
    id: "chatcmpl-mock",
    object: "chat.completion.chunk",
    created: 1,
    model: "mock-model",
    choices: [{ index: 0, delta, finish_reason: null, ...extra }],
  };
}

function sse(res, obj) {
  res.write(`data: ${JSON.stringify(obj)}\n\n`);
}

const server = http.createServer((req, res) => {
  if (req.method === "GET" && req.url.startsWith("/v1/models")) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(
      JSON.stringify({
        object: "list",
        data: [{ id: "mock-model", object: "model", owned_by: "mock" }],
      }),
    );
    return;
  }

  if (req.method === "POST" && req.url.startsWith("/v1/chat/completions")) {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", async () => {
      res.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
      });

      for (const t of THINKING) {
        sse(res, chunk({ role: "assistant", reasoning_content: t }));
        await sleep(THINK_GAP);
      }
      for (let i = 0; i < TEXT.length; i++) {
        sse(res, chunk({ content: TEXT[i] }));
        await sleep(TEXT_GAP);
      }
      sse(res, {
        id: "chatcmpl-mock",
        object: "chat.completion.chunk",
        created: 1,
        model: "mock-model",
        choices: [],
        usage: { prompt_tokens: 42, completion_tokens: 17, total_tokens: 59 },
      });
      // Final event intentionally lacks its terminating blank line.
      sse(res, chunk({ content: " FINALTAIL" }, { finish_reason: "stop" }));
      res.end();
    });
    return;
  }

  res.writeHead(404, { "content-type": "text/plain" });
  res.end("not found");
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`mock-llm listening on http://127.0.0.1:${PORT}/v1`);
});
