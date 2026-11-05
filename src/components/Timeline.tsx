import {
  createEffect,
  createSignal,
  For,
  Index,
  Match,
  Show,
  Switch,
  type Accessor,
  type Setter,
} from "solid-js";
import type { Message } from "../lib/api";
import { DocIcon } from "../lib/icons";
import Markdown from "../lib/Markdown";
import {
  formatBytes,
  parseAttachments,
  type CallEntry,
  type ProcessEntry,
  type TextEntry,
  type ThoughtEntry,
} from "../lib/turns";

/// The reasoning affordance: a pill that opens the full reasoning popup.
///
/// It is deliberately a BUTTON and never the raw trace — the trace lives in the
/// popup, so a turn's activity stays scannable and the container's height does
/// not grow with the model's private reasoning.
function ThoughtRow(props: {
  id: string;
  text: string;
  live: boolean;
  open: boolean;
  onOpen: (id: string, text: string) => void;
}) {
  return (
    <button
      onClick={() => props.onOpen(props.id, props.text)}
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

/// Open/closed state for each turn's activity container lives HERE, not in the
/// component instance. `groupTurns` rebuilds fresh `Turn`/`entries` objects on
/// every streamed delta, and the live turn's `<Timeline>` can be re-mounted
/// (e.g. when a stale reload momentarily drops live rows), so an instance-local
/// signal lost the auto-open and the user's toggle. Keying by the turn's stable
/// id (`Turn.key`) makes the state survive re-renders and remounts.
interface ActivityState {
  get: Accessor<boolean>;
  set: Setter<boolean>;
}

const activityOpen = new Map<string, ActivityState>();
/// The last `live` value observed for a key. The auto-open and the end-of-turn
/// auto-collapse fire on the live→finished EDGE exactly once, never on every
/// render, so a manual mid-stream toggle is not clobbered by incoming deltas.
const activityLive = new Map<string, boolean>();

function activityState(key: string, initial: boolean): ActivityState {
  let state = activityOpen.get(key);
  if (!state) {
    const [get, set] = createSignal(initial);
    state = { get, set };
    activityOpen.set(key, state);
  }
  return state;
}

/// Drop remembered state for turns that are no longer rendered. Called from
/// `MessageList` after each transcript rebuild so the map stays bounded by the
/// visible transcript instead of growing with every turn (or chat) ever opened.
///
/// A live turn is in `renderedKeys` while it is on screen; a chat switched away
/// from mid-turn disappears from the list entirely, which is exactly when its
/// key should be reclaimed (returning to it re-creates the state as an open live
/// panel). The earlier "keep keys whose last observed value was live" guard
/// leaked one entry per mid-turn chat switch forever, because nothing observes
/// the stale key again to clear its flag.
export function pruneActivityState(renderedKeys: readonly string[]): void {
  if (activityOpen.size === 0) return;
  const keep = new Set(renderedKeys);
  for (const key of activityOpen.keys()) {
    if (!keep.has(key)) {
      activityOpen.delete(key);
      activityLive.delete(key);
    }
  }
}

/// One collapsible activity container for a turn: the Thought button(s) that
/// open the reasoning popup, the tool cards, and any interim text the model
/// emitted between them, all in stream order. It opens itself while the turn
/// streams and closes once when it finishes; the user can toggle it at any
/// time, mid-stream included.
///
/// Interim text belongs in here — not out with the answer — because some models
/// reuse their pre-tool narration as a draft and restate it in the final answer.
/// Inline, that duplicated the reply; folded in here it is still visible while
/// the turn runs (the container is open) and is gone once the turn ends. The
/// system prompt also asks the model not to restate it, but that is a nudge, not
/// a guarantee — this is the part that always holds.
function ActivityGroup(props: {
  turnKey: string;
  entries: ProcessEntry[];
  live: boolean;
  openThoughtId: string | null;
  onOpenThought: (id: string, text: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const open = () => activityState(props.turnKey, props.live).get();

  // Track the key this instance last rendered: a live turn's temp user row is
  // replaced by its persisted id when the turn completes, so the component at
  // this index sees the key change. Retire the temp entry instead of leaking it
  // (the new key starts closed, which is exactly the post-turn default).
  let observedKey: string | null = null;
  createEffect(() => {
    const key = props.turnKey;
    const live = props.live;
    if (observedKey !== null && observedKey !== key && observedKey.startsWith("tmp-")) {
      // The live turn's optimistic `tmp-user-*` row was replaced by its
      // persisted id on the completion reload. Retire the temp entry (the new
      // key starts closed, which is exactly the post-turn default); a real→real
      // key change is left alone, since only a temp key is unique to this turn.
      activityOpen.delete(observedKey);
      activityLive.delete(observedKey);
    }
    observedKey = key;
    const state = activityState(key, live);
    const was = activityLive.get(key);
    if (was === live) return;
    activityLive.set(key, live);
    // Auto-open on the finished→live edge; auto-collapse once on live→finished,
    // even if the user expanded or collapsed it mid-stream. A click AFTER the
    // turn finished is a later state change and simply persists.
    if (live) state.set(true);
    else if (was === true) state.set(false);
  });

  const toggle = () => {
    const state = activityState(props.turnKey, props.live);
    state.set((v) => !v);
  };

  const summary = () => {
    const thoughts = props.entries.filter((e) => e.kind === "thought").length;
    const calls = props.entries.filter((e) => e.kind === "call").length;
    const parts: string[] = [];
    if (thoughts) parts.push("Thought");
    if (calls) parts.push(`${calls} tool${calls === 1 ? "" : "s"}`);
    return parts.join(" · ") || "Activity";
  };

  return (
    <div>
      <button
        onClick={toggle}
        title="Show activity timeline"
        aria-expanded={open()}
        class={`flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition ${
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
        <div class="mt-1 space-y-2 border-l-2 border-neutral-200 pl-3 dark:border-neutral-800">
          <Index each={props.entries}>
            {(entry) => (
              <Switch>
                <Match when={entry().kind === "thought"}>
                  <ThoughtRow
                    id={entry().id}
                    text={(entry() as ThoughtEntry).text}
                    live={props.live}
                    open={props.openThoughtId === entry().id}
                    onOpen={props.onOpenThought}
                  />
                </Match>
                <Match when={entry().kind === "text"}>
                  <Markdown text={(entry() as TextEntry).text} />
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
  );
}

/// Stop-reason annotations rendered under the answer. `length` is the model's
/// output limit. (The tool-round cap is gone, so there is no `tool_limit`.)
function stopNote(reason: string | null): string | null {
  switch (reason) {
    case "aborted":
      return "(aborted)";
    case "error":
      return "(error)";
    case "length":
      return "(output limit reached)";
    default:
      return null;
  }
}

/// One turn's assistant side: a single collapsible activity container (thought
/// buttons + tool cards + interim text) followed by the final answer, which
/// closes the sequence carrying the stop-reason annotation and streaming caret.
export function Timeline(props: {
  /// Stable per-turn id (`Turn.key`): scopes the activity container's open
  /// state across re-renders and remounts.
  turnKey: string;
  entries: ProcessEntry[];
  answer: Message | null;
  live: boolean;
  openThoughtId: string | null;
  onOpenThought: (id: string, text: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  const answer = () => props.answer;
  const answerLive = () => props.live && !!answer()?.id.startsWith("tmp-assistant-");

  // Keep the last non-empty entry list while the turn is live. `groupTurns`
  // rebuilds `entries` from scratch on every delta, and a momentary empty pass
  // (a round boundary, or a reload that is about to re-add the live rows) must
  // not unmount the panel — that would drop the open state and the tool cards.
  // Only the transient empty pass is covered: a finished turn renders exactly
  // what `groupTurns` emits, and a key change (temp user row → persisted id)
  // resets the retained list so nothing leaks between turns.
  const [retained, setRetained] = createSignal<ProcessEntry[]>([]);
  let retainedKey: string | null = null;
  createEffect(() => {
    const key = props.turnKey;
    const entries = props.entries;
    if (key !== retainedKey) {
      retainedKey = key;
      setRetained([]);
    }
    if (entries.length > 0) setRetained(entries);
  });
  const activityEntries = () => {
    const entries = props.entries;
    if (entries.length > 0) return entries;
    return props.live ? retained() : entries;
  };

  const shownAnswer = () => {
    const a = answer();
    if (!a) return null;
    const hasText = !!a.content.trim();
    const hasAttachments = parseAttachments(a).length > 0;
    // An aborted/errored/stopped turn keeps its annotation even with no text.
    const stopped = stopNote(a.stop_reason) !== null;
    return hasText || hasAttachments || stopped || answerLive() ? a : null;
  };

  return (
    <div class="flex justify-start">
      <div class="w-full min-w-0 space-y-2 text-sm">
        <Show when={activityEntries().length > 0}>
          <ActivityGroup
            turnKey={props.turnKey}
            entries={activityEntries()}
            live={props.live}
            openThoughtId={props.openThoughtId}
            onOpenThought={props.onOpenThought}
            onApprove={props.onApprove}
            onDeny={props.onDeny}
          />
        </Show>

        <Show when={shownAnswer()}>
          {(a) => (
            <div class="space-y-2" data-message-id={a().id}>
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
              <Show when={stopNote(a().stop_reason)}>
                {(note) => (
                  <p class="text-[11px] text-neutral-500 dark:text-neutral-400">{note()}</p>
                )}
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
