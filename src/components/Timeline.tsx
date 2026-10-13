import { createEffect, createSignal, Index, Match, Show, Switch } from "solid-js";
import Markdown from "../lib/Markdown";
import type { CallEntry, ProcessEntry } from "../lib/turns";

function ThoughtRow(props: {
  id: string;
  live: boolean;
  open: boolean;
  onToggle: (id: string) => void;
}) {
  return (
    <button
      onClick={() => props.onToggle(props.id)}
      title="Show the reasoning trace"
      class={`flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
        props.open
          ? "bg-neutral-200 text-neutral-700 dark:bg-neutral-700 dark:text-neutral-100"
          : "text-neutral-500 hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
      }`}
    >
      <span
        class={`inline-block h-1.5 w-1.5 rounded-full ${
          props.live ? "animate-pulse bg-amber-500" : "bg-neutral-400 dark:bg-neutral-500"
        }`}
      />
      {props.live ? "Thinking…" : "Thought"}
    </button>
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

/// One collapsible activity timeline for a turn: reasoning, preamble text and
/// tool calls/results in order. Expands while live and collapses when done.
export function Timeline(props: {
  entries: ProcessEntry[];
  live: boolean;
  openThoughtId: string | null;
  onToggleThought: (id: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const [open, setOpen] = createSignal(props.live);
  let prevLive = false;
  createEffect(() => {
    const live = props.live;
    if (live) setOpen(true);
    else if (prevLive) setOpen(false);
    prevLive = live;
  });

  const summary = () => {
    const thoughts = props.entries.filter((e) => e.kind === "thought").length;
    const calls = props.entries.filter((e) => e.kind === "call").length;
    const parts: string[] = [];
    if (thoughts) parts.push("Thought");
    if (calls) parts.push(`${calls} tool${calls === 1 ? "" : "s"}`);
    return parts.join(" · ") || "Activity";
  };

  return (
    <div class="flex justify-start">
      <div class="w-full min-w-0">
        <button
          onClick={() => setOpen((o) => !o)}
          title="Show activity timeline"
          class={`mb-1 flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
            open()
              ? "text-neutral-600 dark:text-neutral-300"
              : "text-neutral-500 hover:bg-neutral-200 dark:text-neutral-400 dark:hover:bg-neutral-800"
          }`}
        >
          <span
            class={`inline-block h-1.5 w-1.5 rounded-full ${
              props.live ? "animate-pulse bg-amber-500" : "bg-neutral-400 dark:bg-neutral-500"
            }`}
          />
          <span>{summary()}</span>
          <span class="opacity-60">{open() ? "▾" : "▸"}</span>
        </button>
        <Show when={open()}>
          <div class="space-y-2 border-l-2 border-neutral-200 pl-3 dark:border-neutral-800">
            <Index each={props.entries}>
              {(entry) => (
                <Switch>
                  <Match when={entry().kind === "thought"}>
                    <ThoughtRow
                      id={entry().id}
                      live={props.live}
                      open={props.openThoughtId === entry().id}
                      onToggle={props.onToggleThought}
                    />
                  </Match>
                  <Match when={entry().kind === "text"}>
                    <Markdown
                      text={(entry() as Extract<ProcessEntry, { kind: "text" }>).text}
                      class="prose-sm"
                    />
                  </Match>
                  <Match when={entry().kind === "call"}>
                    <CallRow
                      entry={entry() as CallEntry}
                      onApprove={props.onApprove}
                      onDeny={props.onDeny}
                    />
                  </Match>
                </Switch>
              )}
            </Index>
          </div>
        </Show>
      </div>
    </div>
  );
}
