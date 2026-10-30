import assert from "node:assert/strict";
import test from "node:test";

import { groupTurns, type LiveTool } from "./turns.ts";
import type { Message } from "./api.ts";

// Minimal row builder: only the fields `groupTurns` reads matter.
function row(
  id: string,
  role: string,
  content: string,
  extra: Partial<Message> = {},
): Message {
  return {
    id,
    conversation_id: "c1",
    role,
    index: 0,
    content,
    model: null,
    provider: null,
    thinking_level: null,
    thinking: null,
    usage: null,
    stop_reason: null,
    attachments: null,
    created_at: 0,
    ...extra,
  };
}

/// While a turn streams, the OTHER turns must keep the reasoning entry derived
/// from their answer row. Regression: that entry used to be appended only when
/// nothing was streaming, so a finished turn whose only activity was its
/// answer-level reasoning lost its sole entry — and its whole activity container
/// vanished mid-stream (a tool-using turn lost just the Thought pill).
test("a streaming turn does not drop older turns' answer reasoning", () => {
  const messages: Message[] = [
    row("u1", "user", "first"),
    row("a1", "assistant", "Answer one.", { thinking: "thought one", stop_reason: "stop" }),
    row("u2", "user", "second"),
    row("tmp-assistant-1", "assistant", "", { thinking: "thought two" }),
  ];
  const turns = groupTurns(messages, [], true);

  assert.equal(turns.length, 2);
  const first = turns[0];
  assert.equal(first.live, false);
  assert.ok(
    first.entries.some((e) => e.kind === "thought" && e.id === "a1"),
    "the finished turn keeps its answer-thought entry while the next turn streams",
  );

  // The live turn still trails its own streamed reasoning, exactly once.
  const live = turns[1];
  assert.equal(live.live, true);
  const liveThoughts = live.entries.filter(
    (e) => e.kind === "thought" && e.id === "tmp-assistant-1",
  );
  assert.equal(liveThoughts.length, 1);
});

/// Same input, but no turn is streaming: unchanged behavior.
test("finished turns keep their answer reasoning when idle", () => {
  const messages: Message[] = [
    row("u1", "user", "first"),
    row("a1", "assistant", "Answer one.", { thinking: "thought one", stop_reason: "stop" }),
    row("u2", "user", "second"),
    row("a2", "assistant", "Answer two.", { thinking: "thought two", stop_reason: "stop" }),
  ];
  const turns = groupTurns(messages, [], false);
  for (const [i, id] of ["a1", "a2"].entries()) {
    assert.ok(
      turns[i].entries.some((e) => e.kind === "thought" && e.id === id),
      `turn ${i + 1} has its answer reasoning`,
    );
  }
});

/// A tool round's reasoning stays attached to the round; the answer's trailing
/// thought is added after it, and a live turn's flushed rounds come first.
test("tool rounds and the trailing answer thought coexist in stream order", () => {
  const live: LiveTool = {
    callId: "call_1",
    name: "read_memory",
    arguments: "profile.md",
    gated: false,
    state: "done",
    ok: true,
    output: "ok",
    thinking: "round thought",
    text: "preamble",
  };
  const messages: Message[] = [
    row("u1", "user", "first"),
    row("a0", "assistant", '{"tool_calls":[{"id":"call_0","name":"bash","arguments":{"command":"ls"}}]}', {
      thinking: "round zero",
    }),
    row("t0", "tool", '{"tool_call_id":"call_0","name":"bash","error":false,"output":"file"}'),
    row("a1", "assistant", "Answer one.", { thinking: "final thought", stop_reason: "stop" }),
    row("u2", "user", "second"),
    row("tmp-assistant-2", "assistant", "", {}),
  ];
  const turns = groupTurns(messages, [live], true);
  const first = turns[0];
  const ids = first.entries.map((e) => e.id);
  // Tool round first (thought + preamble + call with its result), answer thought last.
  assert.ok(ids.includes("a0"));
  assert.ok(ids.includes("a0:call_0"));
  assert.ok(ids.includes("a1"));
  assert.ok(
    ids.indexOf("a1") > ids.indexOf("a0:call_0"),
    "the answer thought trails the tool round",
  );

  // The live turn is rebuilt from liveTools + its own trailing reasoning.
  const liveIds = turns[1].entries.map((e) => e.id);
  assert.deepEqual(liveIds, [
    "live:call_1:thought",
    "live:call_1:text",
    "live:call_1",
  ]);
});
