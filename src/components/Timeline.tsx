import { createMemo, createSignal, For, Index, Match, Show, Switch } from "solid-js";
import type { Message } from "../lib/api";
import { DocIcon } from "../lib/icons";
import Markdown from "../lib/Markdown";
import {
  formatBytes,
  groupBlocks,
  parseAttachments,
  type CallEntry,
  type ProcessEntry,
  type ThoughtBlock,
  type ThoughtEntry,
} from "../lib/turns";

/// One collapsible container for a run of consecutive reasoning entries.
///
/// Two controls, deliberately separate:
///   · the chevron toggles the trace inline — local state, so it can be opened
///     and closed at any time, mid-stream included (it is never auto-collapsed
///     or auto-expanded under the user);
///   · the "Thinking…"/"Thought" pill keeps the existing affordance and opens
///     the full reasoning popup.
function ThoughtGroup(props: {
  entries: ThoughtEntry[];
  live: boolean;
  openId: string | null;
  onOpenTrace: (id: string, text: string) => void;
}) {
  const [expanded, setExpanded] = createSignal(false);
  const last = () => props.entries[props.entries.length - 1];
  const open = () => !!last() && props.openId === last()!.id;
  return (
    <div class="text-sm">
      <div class="flex items-center gap-1">
        <button
          onClick={() => setExpanded((o) => !o)}
          title={expanded() ? "Hide reasoning text" : "Show reasoning text"}
          aria-expanded={expanded()}
          class="rounded-md px-1 py-0.5 text-[11px] leading-none text-neutral-500 transition hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          {expanded() ? "▾" : "▸"}
        </button>
        <button
          onClick={() => {
            const entry = last();
            if (entry) props.onOpenTrace(entry.id, entry.text);
          }}
          title="Show the reasoning trace"
          class={`flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
            open()
              ? "bg-neutral-200 text-neutral-700 dark:bg-neutral-700 dark:text-neutral-100"
              : "text-neutral-500 hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
          }`}
        >
          <span
            class={`inline-block h-1.5 w-1.5 rounded-full ${
              props.live
                ? "animate-pulse bg-amber-500"
                : "bg-neutral-400 dark:bg-neutral-500"
            }`}
          />
          {props.live ? "Thinking…" : "Thought"}
        </button>
      </div>
      <Show when={expanded()}>
        <div class="mt-1 space-y-2 border-l-2 border-neutral-200 pl-3 dark:border-neutral-800">
          <For each={props.entries}>
            {(entry) => (
              <div class="whitespace-pre-wrap break-words text-xs leading-relaxed text-neutral-500 dark:text-neutral-400">
                {entry.text}
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function CallRow(props: {
  entry: CallEntry;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const live = () => props.entry.live;
  const state = () => live()?.state;
  return (
    <div class="rounded-lg border border-neutral-200 bg-white px-3 py-2 text-xs dark:border-neutral-800 dark:bg-neutral-900">
      <div class="mb-1 text-[11px] text-neutral-500 dark:text-neutral-400">
        {state() === "pending" && "approval needed · "}
        {state() === "running" && "running · "}
        {state() === "done" && (live()?.ok ? "done · " : "error · ")}
        <span class="font-medium text-neutral-700 dark:text-neutral-200">{props.entry.name}</span>
      </div>
      <pre class="max-h-40 overflow-auto whitespace-pre-wrap break-words text-neutral-600 dark:text-neutral-400">
        {props.entry.arguments}
      </pre>
      <Show when={state() === "pending"}>
        <div class="mt-2 flex gap-2">
          <button
            class="rounded-md bg-neutral-900 px-3 py-1 text-xs font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
            onClick={() => props.onApprove(props.entry.callId)}
          >
            Approve
          </button>
          <button
            class="rounded-md border border-neutral-300 px-3 py-1 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
            onClick={() => props.onDeny(props.entry.callId)}
          >
            Deny
          </button>
        </div>
      </Show>
      <Show when={props.entry.result}>
        <div class="mt-2">
          <div class="text-[11px] text-neutral-500 dark:text-neutral-400">
            {props.entry.result!.error ? "error" : "result"}
          </div>
          <pre class="mt-0.5 max-h-60 overflow-auto whitespace-pre-wrap break-words text-neutral-700 dark:text-neutral-300">
            {props.entry.result!.output}
          </pre>
        </div>
      </Show>
    </div>
  );
}

/// One turn's assistant side, in stream order: reasoning containers, output
/// segments and tool cards interleaved exactly as they were produced, so the
/// whole thing reads as ONE continuous reply instead of a hidden activity log
/// plus a single answer bubble. The final text (the answer row) closes the
/// sequence, carrying the stop-reason annotation and the streaming caret.
export function Timeline(props: {
  entries: ProcessEntry[];
  answer: Message | null;
  live: boolean;
  openThoughtId: string | null;
  onOpenThought: (id: string, text: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const blocks = createMemo(() => groupBlocks(props.entries));
  const answer = () => props.answer;
  const answerLive = () => props.live && !!answer()?.id.startsWith("tmp-assistant-");
  const shownAnswer = () => {
    const a = answer();
    if (!a) return null;
    const hasText = !!a.content.trim();
    const hasAttachments = parseAttachments(a).length > 0;
    return hasText || hasAttachments || answerLive() ? a : null;
  };

  return (
    <div class="flex justify-start">
      <div class="w-full min-w-0 space-y-2 text-sm">
        <Index each={blocks()}>
          {(block, index) => (
            <Switch>
              <Match when={block().kind === "thoughts"}>
                <ThoughtGroup
                  entries={(block() as ThoughtBlock).entries}
                  live={props.live && index === blocks().length - 1}
                  openId={props.openThoughtId}
                  onOpenTrace={props.onOpenThought}
                />
              </Match>
              <Match when={block().kind === "text"}>
                <Markdown text={(block() as { kind: "text"; entry: { text: string } }).entry.text} />
              </Match>
              <Match when={block().kind === "call"}>
                <CallRow
                  entry={(block() as { kind: "call"; entry: CallEntry }).entry}
                  onApprove={props.onApprove}
                  onDeny={props.onDeny}
                />
              </Match>
            </Switch>
          )}
        </Index>

        <Show when={shownAnswer()}>
          {(a) => (
            <div class="space-y-2">
              <Show when={parseAttachments(a()).length > 0}>
                <div class="flex flex-wrap items-end gap-2">
                  <For each={parseAttachments(a())}>
                    {(att) =>
                      att.kind === "image" && att.dataUrl ? (
                        <img
                          src={att.dataUrl}
                          alt={att.name}
                          class="max-h-64 max-w-full cursor-zoom-in rounded-lg border border-neutral-300 dark:border-neutral-700"
                        />
                      ) : (
                        <div class="flex items-center gap-2 rounded-lg border border-neutral-300 bg-neutral-100 px-2.5 py-1.5 text-xs dark:border-neutral-700 dark:bg-neutral-800">
                          <DocIcon />
                          <span class="max-w-52 truncate">{att.name}</span>
                          <span class="opacity-60">{formatBytes(att.size)}</span>
                        </div>
                      )
                    }
                  </For>
                </div>
              </Show>
              <Show when={a().content.trim()}>
                <Markdown text={a().content} />
              </Show>
              <Show when={a().stop_reason === "aborted" || a().stop_reason === "error"}>
                <p class="text-[11px] text-neutral-500 dark:text-neutral-400">
                  {a().stop_reason === "aborted" ? "(aborted)" : "(error)"}
                </p>
              </Show>
              <Show when={answerLive()}>
                <span class="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-neutral-400 align-text-bottom" />
              </Show>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}
