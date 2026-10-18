import type { Attachment, Message, Pricing } from "./api";

export interface LiveTool {
  callId: string;
  name: string;
  arguments: string;
  gated: boolean;
  state: "pending" | "running" | "done";
  ok?: boolean;
  output?: string;
  /// Streamed reasoning that preceded this call's round, captured when the
  /// call arrived. Mirrors the reasoning the backend persists on the round's
  /// preamble row (or its tool-call row when there is no preamble text).
  thinking?: string;
  /// Streamed preamble text for this call's round, captured when the call
  /// arrived. Mirrors the backend's `assistant(text)` row so the live and
  /// reloaded timelines interleave identically.
  text?: string;
}

export type ProcessEntry =
  | { kind: "thought"; id: string; text: string }
  | { kind: "text"; id: string; text: string }
  | {
      kind: "call";
      id: string;
      callId: string;
      name: string;
      arguments: string;
      result: { output: string; error: boolean } | null;
      live?: LiveTool;
    };

export type CallEntry = Extract<ProcessEntry, { kind: "call" }>;
export type ThoughtEntry = Extract<ProcessEntry, { kind: "thought" }>;
export type TextEntry = Extract<ProcessEntry, { kind: "text" }>;

export interface Turn {
  key: string;
  user: Message | null;
  entries: ProcessEntry[];
  answer: Message | null;
  live: boolean;
  /// A memory-consolidation note attached to this turn (background bookkeeping).
  note: Message | null;
}

export function parseToolResult(
  m: Message,
): { name: string; output: string; error: boolean; callId: string | null } | null {
  if (m.role !== "tool") return null;
  try {
    const v = JSON.parse(m.content);
    if (v && typeof v === "object" && "tool_call_id" in v && "output" in v) {
      return {
        name: String(v.name ?? "tool"),
        output: String(v.output),
        error: Boolean(v.error),
        callId: v.tool_call_id != null ? String(v.tool_call_id) : null,
      };
    }
  } catch {
    // fall through
  }
  return { name: "tool", output: m.content, error: false, callId: null };
}

export function formatToolArgs(raw: unknown): string {
  let value = raw;
  if (typeof raw === "string") {
    try {
      value = JSON.parse(raw);
    } catch {
      return raw; // partial/invalid JSON while streaming
    }
  }
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const entries = Object.entries(value as Record<string, unknown>);
    // Single string field (e.g. bash's `{command}`) → show just the value.
    if (entries.length === 1 && typeof entries[0][1] === "string") {
      return entries[0][1] as string;
    }
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }
  return String(value ?? "");
}

export function parseToolCallList(
  m: Message,
): { id: string; name: string; arguments: unknown }[] | null {
  if (m.role !== "assistant") return null;
  try {
    const v = JSON.parse(m.content);
    if (v && typeof v === "object" && Array.isArray(v.tool_calls)) {
      return v.tool_calls.map((c: any) => ({
        id: String(c?.id ?? c?.name ?? ""),
        name: String(c?.name ?? ""),
        arguments: c?.arguments,
      }));
    }
  } catch {
    // fall through
  }
  return null;
}

export function parseUsage(m: Message): Record<string, number> | null {
  if (!m.usage) return null;
  try {
    const v = JSON.parse(m.usage);
    return v && typeof v === "object" ? (v as Record<string, number>) : null;
  } catch {
    return null;
  }
}

export function parseAttachments(m: Message): Attachment[] {
  if (!m.attachments) return [];
  try {
    const v = JSON.parse(m.attachments);
    return Array.isArray(v) ? (v as Attachment[]) : [];
  } catch {
    return [];
  }
}

export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export function formatTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`;
  return String(n);
}

export function formatCost(cost: number): string {
  if (!Number.isFinite(cost) || cost <= 0) return "$0";
  if (cost < 0.01) return `$${cost.toFixed(4)}`;
  if (cost < 1) return `$${cost.toFixed(3)}`;
  return `$${cost.toFixed(2)}`;
}

/// Cost of one turn's summed usage at the given model rates (mirrors
/// `pricing::cost_of_usage`). Returns null when the model has no rates.
export function costOfUsage(u: Record<string, number>, p: Pricing): number | null {
  if (p.inputPerMillion == null || p.outputPerMillion == null) return null;
  const input = p.inputPerMillion;
  const output = p.outputPerMillion;
  const cacheRead = p.cacheReadPerMillion ?? input;
  const cacheWrite = p.cacheWritePerMillion ?? input;
  const prompt = u.prompt_tokens ?? 0;
  const cached = Math.min(u.cached_tokens ?? 0, prompt);
  const cacheWriteTokens = Math.min(u.cache_write_tokens ?? 0, Math.max(0, prompt - cached));
  const uncached = Math.max(0, prompt - cached - cacheWriteTokens);
  return (
    (uncached * input +
      cached * cacheRead +
      cacheWriteTokens * cacheWrite +
      (u.completion_tokens ?? 0) * output) /
    1_000_000
  );
}

/// Turn a turn's process rows into an ordered list of timeline entries,
/// pairing each tool call with the result row that follows it.
export function buildEntries(rows: Message[]): ProcessEntry[] {
  const entries: ProcessEntry[] = [];
  const calls = new Map<string, CallEntry>();
  for (const m of rows) {
    if (m.thinking?.trim()) entries.push({ kind: "thought", id: m.id, text: m.thinking });
    const toolCalls = parseToolCallList(m);
    if (toolCalls) {
      for (const c of toolCalls) {
        const entry: CallEntry = {
          kind: "call",
          id: `${m.id}:${c.id}`,
          callId: c.id,
          name: c.name,
          arguments: formatToolArgs(c.arguments),
          result: null,
        };
        entries.push(entry);
        calls.set(c.id, entry);
      }
      continue;
    }
    const tr = parseToolResult(m);
    if (tr) {
      const entry = tr.callId ? calls.get(tr.callId) : undefined;
      if (entry) entry.result = { output: tr.output, error: tr.error };
      else if (tr.output.trim()) entries.push({ kind: "text", id: m.id, text: tr.output });
      continue;
    }
    if (m.content.trim()) entries.push({ kind: "text", id: m.id, text: m.content });
  }
  return entries;
}

/// Group a flat message list into turns: a user row opens a turn, the final
/// non-user row is the answer, and everything before it is process activity.
/// While streaming, the live turn's entries come from `liveTools` plus the
/// in-flight assistant's reasoning instead.
export function groupTurns(messages: Message[], liveTools: LiveTool[], streaming: boolean): Turn[] {
  const turns: Turn[] = [];
  const rowsByTurn = new Map<Turn, Message[]>();
  let current: Turn | null = null;
  const flush = () => {
    if (current) turns.push(current);
    current = null;
  };
  for (const m of messages) {
    if (m.role === "user") {
      flush();
      current = { key: m.id, user: m, entries: [], answer: null, live: false, note: null };
      rowsByTurn.set(current, []);
      continue;
    }
    // Clock notes are hidden bookkeeping rows: they belong in the model's context
    // (and in the shared prompt prefix), never in the timeline.
    if (m.role === "clock") continue;
    if (m.role === "memory") {
      if (!current) {
        current = { key: m.id, user: null, entries: [], answer: null, live: false, note: m };
        rowsByTurn.set(current, []);
      } else {
        current.note = m;
      }
      continue;
    }
    if (!current) {
      current = { key: m.id, user: null, entries: [], answer: null, live: false, note: null };
      rowsByTurn.set(current, []);
    }
    rowsByTurn.get(current)!.push(m);
  }
  flush();

  for (const turn of turns) {
    const rows = rowsByTurn.get(turn) ?? [];
    const last = rows[rows.length - 1];
    let answer: Message | null = null;
    let processRows = rows;
    if (last && last.role === "assistant" && !parseToolCallList(last) && !parseToolResult(last)) {
      answer = last;
      processRows = rows.slice(0, -1);
    }
    turn.answer = answer;
    turn.entries = buildEntries(processRows);
  }

  const live = turns[turns.length - 1];
  if (live && streaming && messages.some((m) => m.id.startsWith("tmp-assistant-"))) {
    live.live = true;
    live.entries = [];
    // Live rounds are rebuilt to match what `buildEntries` will derive once the
    // turn is persisted: for each tool round, the streamed reasoning + preamble
    // text land as entries *before* that round's call(s). App captures that
    // preamble onto the first call of the round when the call arrives and resets
    // the in-flight assistant message, so the entries stay in arrival order and
    // are append-only (the `Index`-keyed DOM nodes at each position survive).
    for (const t of liveTools) {
      if (t.thinking?.trim()) {
        live.entries.push({
          kind: "thought",
          id: `live:${t.callId}:thought`,
          text: t.thinking,
        });
      }
      if (t.text?.trim()) {
        live.entries.push({ kind: "text", id: `live:${t.callId}:text`, text: t.text });
      }
      live.entries.push({
        kind: "call",
        id: `live:${t.callId}`,
        callId: t.callId,
        name: t.name,
        arguments: t.arguments,
        result: t.state === "done" ? { output: t.output ?? "", error: !t.ok } : null,
        live: t,
      });
    }
    // The current (not-yet-flushed) round's reasoning trails the flushed ones;
    // its text stays on the answer bubble ("text remaining at done is the
    // answer"). Keeping it last means its DOM node survives as later rounds
    // append entries after it.
    const thought = live.answer?.id.startsWith("tmp-assistant-")
      ? live.answer?.thinking?.trim()
      : null;
    if (thought && live.answer) {
      live.entries.push({ kind: "thought", id: live.answer.id, text: thought });
    }
  } else {
    // Persisted turns: the answer row's reasoning is a trailing timeline entry.
    for (const turn of turns) {
      if (turn.answer?.thinking?.trim()) {
        turn.entries.push({ kind: "thought", id: turn.answer.id, text: turn.answer.thinking });
      }
    }
  }
  return turns;
}
