import { createEffect, For, Show } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import type {
  BackfillStatus,
  ExtractionStats,
  ImportReport,
  MemoryFile,
  ModelInfo,
  ReflectionStats,
  ThinkingOptions,
} from "../lib/api";
import Markdown from "../lib/Markdown";
import { BackArrowIcon } from "../lib/icons";
import { formatCost } from "../lib/turns";

/// The Memory dialog: the file list + editor/preview pane, and the
/// Imported-chats / backfill card.
export function MemoryPage(props: {
  memoryOpen: boolean;
  setMemoryOpen: (open: boolean) => void;
  wideViewport: boolean;
  fullscreenInsetStyle: () => Record<string, string>;
  memoryFiles: MemoryFile[];
  memorySelected: string | null;
  selectMemoryFile: (file: MemoryFile) => void;
  selectedMemoryFile: MemoryFile | null;
  memoryDraft: string;
  setMemoryDraft: (value: string) => void;
  memoryPreview: boolean;
  setMemoryPreview: (updater: (prev: boolean) => boolean) => void;
  memoryConfirmDelete: string | null;
  setMemoryConfirmDelete: (name: string | null) => void;
  removeMemoryFile: (name: string) => void;
  saveSelectedMemory: () => void;
  memoryError: string | null;
  importReport: ImportReport | null;
  extractionStats: ExtractionStats | null;
  importChats: () => void;
  discardExtractions: () => void;
  backfill: BackfillStatus | null;
  backfillModels: ModelInfo[];
  backfillModel: string;
  setBackfillModel: (value: string) => void;
  backfillThinking: string;
  setBackfillThinking: (value: string) => void;
  backfillThinkingOpts: ThinkingOptions | null;
  startBackfill: (opts?: {
    model?: string | null;
    thinkingLevel?: string | null;
    allHistory?: boolean;
    force?: boolean;
  }) => void;
  stopBackfill: () => void;
  activeId: string | null;
  consolidating: boolean;
  consolidateNow: () => void;
  reflectionStats: ReflectionStats | null;
}) {
  // WebKit clears a <select> when its options rebuild, and the `value` binding
  // won't restore `backfillModel()` because the signal is unchanged — so the
  // picker "reverts to default" after backfillModels() reloads. Force the DOM
  // value back whenever the selection or the option list changes.
  let backfillModelSelectEl: HTMLSelectElement | undefined;
  createEffect(() => {
    props.backfillModels;
    props.backfillModel;
    if (backfillModelSelectEl && backfillModelSelectEl.value !== props.backfillModel) {
      backfillModelSelectEl.value = props.backfillModel;
    }
  });

  return (
    <Dialog open={props.memoryOpen} onOpenChange={props.setMemoryOpen}>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
        <Dialog.Content
          class={
            // On narrow (mobile) viewports — the same `lg` threshold that turns
            // the chat list into a drawer — expand to cover the whole viewport
            // so these pages don't render as a cramped centered card.
            props.wideViewport
              ? "fixed left-1/2 top-1/2 z-50 flex h-[70vh] w-full max-w-3xl -translate-x-1/2 -translate-y-1/2 flex-col rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
              : "fixed inset-0 z-50 flex h-full w-full flex-col overflow-y-auto bg-white p-5 focus:outline-none dark:bg-neutral-900"
          }
          style={props.fullscreenInsetStyle()}
        >
          <div class="flex items-center gap-2">
            {/* Mobile (full-screen) exit arrow — hidden on wide viewports, where
                the centered card keeps its Close button. */}
            <Dialog.CloseButton
              title="Close memory"
              aria-label="Close memory"
              class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              <BackArrowIcon />
            </Dialog.CloseButton>
            <Dialog.Title class="text-base font-semibold">Memory</Dialog.Title>
          </div>
          <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
            Long-term memory is plain Markdown files. Core files (profile, preferences,
            goals) are always shown to the assistant; other files are listed and read on
            demand. The idle reflection pass curates these files. Explicit user preferences
            are set in Settings and kept separate.
          </Dialog.Description>

          <div class="mt-4 flex min-h-0 flex-1 gap-4">
            <div class="flex w-44 shrink-0 flex-col gap-1 overflow-y-auto">
              <p class="shrink-0 px-2 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                Core
              </p>
              <For each={props.memoryFiles.filter((f) => f.core)}>
                {(f) => (
                  <button
                    onClick={() => props.selectMemoryFile(f)}
                    class={`truncate shrink-0 rounded-md px-2 py-1.5 text-left text-xs transition ${
                      props.memorySelected === f.name
                        ? "bg-neutral-200 font-medium dark:bg-neutral-800"
                        : "text-neutral-600 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                    }`}
                  >
                    {f.name}
                  </button>
                )}
              </For>
              <Show when={props.memoryFiles.some((f) => !f.core)}>
                <p class="mt-3 shrink-0 px-2 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                  Other
                </p>
                <For each={props.memoryFiles.filter((f) => !f.core)}>
                  {(f) => (
                    <button
                      onClick={() => props.selectMemoryFile(f)}
                      class={`truncate shrink-0 rounded-md px-2 py-1.5 text-left text-xs transition ${
                        props.memorySelected === f.name
                          ? "bg-neutral-200 font-medium dark:bg-neutral-800"
                          : "text-neutral-600 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      }`}
                    >
                      {f.name}
                    </button>
                  )}
                </For>
              </Show>
            </div>

            <div class="flex min-w-0 flex-1 flex-col">
              <Show
                when={props.selectedMemoryFile}
                fallback={
                  <p class="text-sm text-neutral-400 dark:text-neutral-500">
                    No memory file selected.
                  </p>
                }
              >
                <Show
                  when={!props.memoryPreview}
                  fallback={
                    <div class="min-h-0 flex-1 overflow-y-auto rounded-lg border border-neutral-300 bg-white p-4 dark:border-neutral-700 dark:bg-neutral-950">
                      <Show
                        when={props.memoryDraft.trim()}
                        fallback={
                          <p class="text-xs text-neutral-400 dark:text-neutral-500">
                            (empty)
                          </p>
                        }
                      >
                        <Markdown text={props.memoryDraft} class="prose-sm" />
                      </Show>
                    </div>
                  }
                >
                  <textarea
                    class="min-h-0 flex-1 resize-none rounded-lg border border-neutral-300 bg-white p-3 font-mono text-xs outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-950 dark:text-neutral-100"
                    spellcheck={false}
                    value={props.memoryDraft}
                    onInput={(e) => props.setMemoryDraft(e.currentTarget.value)}
                  />
                </Show>
                <div class="mt-3 flex flex-wrap items-center justify-between gap-3">
                  <Show
                    when={props.memoryConfirmDelete === props.selectedMemoryFile?.name}
                    fallback={
                      <button
                        onClick={() =>
                          props.setMemoryConfirmDelete(props.selectedMemoryFile?.name ?? null)
                        }
                        class="text-[11px] text-red-600 transition hover:underline dark:text-red-400"
                      >
                        Delete file
                      </button>
                    }
                  >
                    <div class="flex items-center gap-2">
                      <span class="text-[11px] text-neutral-500 dark:text-neutral-400">
                        Delete {props.selectedMemoryFile?.name}?
                      </span>
                      <button
                        onClick={() =>
                          void props.removeMemoryFile(props.selectedMemoryFile?.name ?? "")
                        }
                        class="rounded bg-red-600 px-2 py-1 text-[11px] text-white transition hover:bg-red-500"
                      >
                        Delete
                      </button>
                      <button
                        onClick={() => props.setMemoryConfirmDelete(null)}
                        class="text-[11px] text-neutral-500 transition hover:underline dark:text-neutral-400"
                      >
                        Cancel
                      </button>
                    </div>
                  </Show>

                  <div class="flex items-center gap-2">
                    <Show when={props.memoryError}>
                      <span class="max-w-64 whitespace-pre-wrap break-words text-[11px] text-red-600 dark:text-red-400">
                        {props.memoryError}
                      </span>
                    </Show>
                    <button
                      onClick={() => props.setMemoryPreview((p) => !p)}
                      class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                    >
                      {props.memoryPreview ? "Edit" : "Preview"}
                    </button>
                    <button
                      onClick={() => void props.saveSelectedMemory()}
                      class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                    >
                      Save
                    </button>
                  </div>
                </div>
              </Show>
            </div>
          </div>

          {/* Import an archive, then backfill it: map (parallel extraction)
              then reduce (one writer). */}
          <div class="mt-4 rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
            <div class="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
              <div class="min-w-0">
                <p class="text-xs font-medium">Imported chats</p>
                <p class="mt-0.5 text-[11px] leading-relaxed text-neutral-500 dark:text-neutral-400">
                  Import an Anthropic export — a <code>conversations.json</code> file.
                  Imported chats keep their original dates and are kept out of the
                  idle reflection sweep. Backfill then extracts durable facts from
                  them in parallel and consolidates the summaries into memory in one
                  pass; extraction never writes to memory, only the final pass does.
                  Backfill covers only your most recent import — or all of
                  history when you choose "Backfill all history". The run uses
                  the active provider's default model unless you pick one
                  below.
                </p>
                <div class="mt-2 flex flex-wrap items-center gap-3">
                  <label class="flex items-center gap-1.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                    Backfill model
                    <select
                      ref={(el) => (backfillModelSelectEl = el)}
                      class="rounded-md border border-neutral-300 bg-white px-2 py-1 text-[11px] text-neutral-700 outline-none focus:border-neutral-500 dark:border-neutral-600 dark:bg-neutral-800 dark:text-neutral-200"
                      value={props.backfillModel}
                      disabled={props.backfill?.running}
                      onChange={(e) => {
                        props.setBackfillModel(e.currentTarget.value);
                        // Drop a thinking level the new model doesn't accept.
                        if (
                          props.backfillThinking &&
                          props.backfillThinkingOpts &&
                          !props.backfillThinkingOpts.options.includes(props.backfillThinking)
                        ) {
                          props.setBackfillThinking("");
                        }
                      }}
                    >
                      <option value="">Active default</option>
                      <For each={props.backfillModels}>
                        {(m) => <option value={m.id}>{m.name?.trim() || m.id}</option>}
                      </For>
                    </select>
                  </label>
                  <Show
                    when={
                      props.backfillThinkingOpts?.supportsReasoning &&
                      props.backfillThinkingOpts.options.length > 0
                    }
                  >
                    <label class="flex items-center gap-1.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                      Thinking
                      <select
                        class="rounded-md border border-neutral-300 bg-white px-2 py-1 text-[11px] text-neutral-700 outline-none focus:border-neutral-500 dark:border-neutral-600 dark:bg-neutral-800 dark:text-neutral-200"
                        value={props.backfillThinking}
                        disabled={props.backfill?.running}
                        onChange={(e) => props.setBackfillThinking(e.currentTarget.value)}
                      >
                        <option value="">Provider default</option>
                        <For each={props.backfillThinkingOpts!.options}>
                          {(o) => <option value={o}>Thinking: {o}</option>}
                        </For>
                      </select>
                    </label>
                  </Show>
                </div>
                <Show when={props.importReport}>
                  <p class="mt-1 text-[11px] text-neutral-400 dark:text-neutral-500">
                    Imported {props.importReport!.conversations} chats ·{' '}
                    {props.importReport!.messages} messages
                    <Show when={props.importReport!.skipped > 0}>
                      {' '}
                      · {props.importReport!.skipped} skipped
                    </Show>
                  </p>
                </Show>
                <Show when={props.extractionStats}>
                  <p class="mt-1 text-[11px] text-neutral-400 dark:text-neutral-500">
                    {props.extractionStats!.staged} staged · {props.extractionStats!.pending}{' '}
                    awaiting extraction
                  </p>
                </Show>
              </div>
              <div class="flex shrink-0 flex-wrap items-center justify-end gap-2">
                <button
                  onClick={() => void props.importChats()}
                  disabled={props.backfill?.running}
                  class="rounded-md border border-neutral-300 px-2.5 py-1.5 text-[11px] text-neutral-600 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-300 dark:hover:bg-neutral-800"
                >
                  Import file…
                </button>
                <Show when={props.extractionStats?.staged}>
                  <button
                    onClick={() => void props.discardExtractions()}
                    disabled={props.backfill?.running}
                    class="rounded-md border border-neutral-300 px-2.5 py-1.5 text-[11px] text-neutral-600 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-300 dark:hover:bg-neutral-800"
                  >
                    Discard staged
                  </button>
                </Show>
                <Show
                  when={props.backfill?.running}
                  fallback={
                    <button
                      onClick={() =>
                        void props.startBackfill({
                          model: props.backfillModel || null,
                          thinkingLevel: props.backfillThinking || null,
                        })
                      }
                      class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                    >
                      Backfill
                    </button>
                  }
                >
                  <button
                    onClick={() => void props.stopBackfill()}
                    class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                  >
                    Cancel
                  </button>
                </Show>
                <button
                  onClick={() =>
                    void props.startBackfill({
                      allHistory: true,
                      model: props.backfillModel || null,
                      thinkingLevel: props.backfillThinking || null,
                    })
                  }
                  disabled={props.backfill?.running}
                  class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                >
                  Backfill all history
                </button>
                <button
                  onClick={() =>
                    void props.startBackfill({
                      force: true,
                      model: props.backfillModel || null,
                      thinkingLevel: props.backfillThinking || null,
                    })
                  }
                  disabled={props.backfill?.running}
                  title="Re-extract every conversation even if it was already backfilled (ignores the 'already extracted' watermark), then consolidate into memory. A full re-mine — slower than 'Backfill all history'."
                  class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                >
                  Force backfill all history
                </button>
              </div>
            </div>

            <Show when={props.backfill?.running}>
              <div class="mt-2">
                <div class="h-1 w-full overflow-hidden rounded-full bg-neutral-200 dark:bg-neutral-700">
                  <div
                    class="h-full rounded-full bg-neutral-900 transition-all dark:bg-white"
                    style={{
                      width: `${
                        props.backfill!.total > 0
                          ? Math.round((props.backfill!.done / props.backfill!.total) * 100)
                          : 0
                      }%`,
                    }}
                  />
                </div>
                <p class="mt-1 text-[11px] text-neutral-500 dark:text-neutral-400">
                  {props.backfill!.phase} — {props.backfill!.done}/{props.backfill!.total}
                  <Show when={props.backfill!.concurrency > 0}>
                    {' '}
                    · {props.backfill!.concurrency} in flight
                  </Show>
                  <Show when={props.backfill!.failed > 0}>
                    {' '}
                    · {props.backfill!.failed} failed
                  </Show>
                </p>
              </div>
            </Show>
            <Show when={props.backfill?.lastError}>
              <p
                class="mt-1 max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-md bg-red-50 p-2 text-[11px] leading-relaxed text-red-600 dark:bg-red-950/40 dark:text-red-400"
              >
                last error: {props.backfill!.lastError}
              </p>
            </Show>
          </div>

          <div class="mt-4 flex items-center justify-between gap-3">
            <div class="flex items-center gap-3">
              <button
                onClick={() => void props.consolidateNow()}
                disabled={!props.activeId || props.consolidating}
                title={
                  props.activeId
                    ? "Run the memory consolidation pass over the open conversation now"
                    : "Open a conversation first"
                }
                class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
              >
                {props.consolidating ? "Consolidating…" : "Consolidate this chat"}
              </button>
              <Show when={props.reflectionStats && props.reflectionStats.count > 0}>
                <span class="text-[11px] text-neutral-400 dark:text-neutral-500">
                  Reflection spend: {formatCost(props.reflectionStats!.cost)} across{" "}
                  {props.reflectionStats!.count}{" "}
                  {props.reflectionStats!.count === 1 ? "run" : "runs"}
                </span>
              </Show>
            </div>
            <Dialog.CloseButton class="rounded-md border border-neutral-300 px-4 py-1.5 text-sm text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800">
              Close
            </Dialog.CloseButton>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog>
  );
}
