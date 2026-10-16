import { createEffect, For, Show } from "solid-js";
import type { Attachment } from "../lib/api";
import { DocIcon } from "../lib/icons";
import { isAndroid } from "../lib/platform";
import { formatBytes } from "../lib/turns";

/// The composer: staged attachments, the near-limit / vision warnings, the
/// textarea and the send/stop controls. The chat box is a single rounded
/// container with the attach button bottom-left, send/stop bottom-right, and
/// an auto-growing textarea filling the middle (~3rem -> 60vh/14 lines, then scroll).
export function Composer(props: {
  pendingAttachments: Attachment[];
  onRemoveAttachment: (id: string) => void;
  contextNote: string | null;
  attachmentWarning: string | null;
  streaming: boolean;
  busy?: boolean;
  onPickAttachments: () => void;
  draft: string;
  setDraft: (value: string) => void;
  onSend: () => void;
  onStop: () => void;
}) {
  let textareaEl: HTMLTextAreaElement | undefined;
  const busy = () => props.busy ?? props.streaming;

  // Return focus to the composer once a turn finishes (including after a
  // Stop), so the chatbox is immediately usable again. Skip on Android:
  // focusing there pops the on-screen keyboard after the agent finishes
  // typing (issue 6).
  let prevStreaming = props.streaming;
  createEffect(() => {
    const s = props.streaming;
    if (prevStreaming && !s && !isAndroid) textareaEl?.focus();
    prevStreaming = s;
  });

  const autoResize = () => {
    const el = textareaEl;
    if (!el) return;
    el.style.height = "auto";
    const cap = typeof window !== "undefined" ? window.innerHeight * 0.6 : 360;
    const next = Math.min(el.scrollHeight, cap);
    el.style.height = next + "px";
    el.style.overflowY = el.scrollHeight > cap ? "auto" : "hidden";
  };

  // Re-size when the controlled draft changes externally (e.g. parent clears
  // after send, or loads a saved draft).
  createEffect(() => {
    void props.draft;
    queueMicrotask(autoResize);
  });

  const handleSend = () => {
    props.onSend();
    queueMicrotask(autoResize);
  };

  return (
    <footer class="border-t border-neutral-200 p-4 dark:border-neutral-800">
      <Show when={props.pendingAttachments.length > 0}>
        <div class="mb-2 flex flex-wrap items-center gap-2">
          <For each={props.pendingAttachments}>
            {(a) => (
              <div class="relative">
                <Show
                  when={a.kind === "image" && a.dataUrl}
                  fallback={
                    <div class="flex items-center gap-2 rounded-lg border border-neutral-300 bg-neutral-100 px-2.5 py-1.5 text-xs dark:border-neutral-700 dark:bg-neutral-800">
                      <DocIcon />
                      <span class="max-w-52 truncate">{a.name}</span>
                      <span class="opacity-60">{formatBytes(a.size)}</span>
                    </div>
                  }
                >
                  <img
                    src={a.dataUrl ?? ""}
                    alt={a.name}
                    class="h-16 w-16 rounded-lg border border-neutral-300 object-cover dark:border-neutral-700"
                  />
                </Show>
                <button
                  onClick={() => props.onRemoveAttachment(a.id)}
                  title="Remove attachment"
                  class="absolute -right-1.5 -top-1.5 flex h-5 w-5 items-center justify-center rounded-full bg-neutral-900 text-xs leading-none text-white shadow transition hover:bg-red-600 dark:bg-white dark:text-neutral-900 dark:hover:bg-red-400"
                >
                  ×
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
      <Show when={props.contextNote}>
        <p class="mb-2 rounded-lg border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-700 dark:bg-amber-950/40 dark:text-amber-300">
          {props.contextNote}
        </p>
      </Show>
      <Show when={props.attachmentWarning}>
        <p
          aria-live="polite"
          class="mb-2 rounded-lg border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-700 dark:bg-amber-950/40 dark:text-amber-300"
        >
          {props.attachmentWarning}
        </p>
      </Show>
      <div class="flex flex-col rounded-[28px] border border-neutral-300 bg-white p-3 shadow-sm focus-within:border-neutral-400 dark:border-neutral-700 dark:bg-neutral-900 dark:focus-within:border-neutral-600">
        <textarea
          ref={(el) => {
            textareaEl = el;
            queueMicrotask(autoResize);
          }}
          class="max-h-[60vh] min-h-[3rem] w-full resize-none bg-transparent px-2 py-1 text-sm leading-6 outline-none placeholder:text-neutral-400 dark:text-neutral-100 dark:placeholder:text-neutral-500"
          rows={1}
          placeholder={props.streaming ? "Assistant is replying…" : "Message Pi Chat…"}
          value={props.draft}
          disabled={busy()}
          onInput={(e) => {
            props.setDraft(e.currentTarget.value);
            autoResize();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !isAndroid) {
              e.preventDefault();
              handleSend();
            }
          }}
        />
        <div class="mt-2 flex items-center justify-between gap-2">
          <button
            onClick={() => void props.onPickAttachments()}
            disabled={busy()}
            title="Attach files"
            aria-label="Attach files"
            class="flex h-8 w-8 shrink-0 items-center justify-center rounded-full border border-neutral-200 bg-neutral-50 text-neutral-600 transition hover:bg-neutral-100 disabled:opacity-40 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-300 dark:hover:bg-neutral-700"
          >
            <span class="text-[18px] leading-none">+</span>
          </button>
          <div class="flex shrink-0 items-center gap-2">
            <Show
              when={props.streaming}
              fallback={
                <button
                  onClick={handleSend}
                  disabled={busy() || (!props.draft.trim() && props.pendingAttachments.length === 0)}
                  title="Send"
                  aria-label="Send"
                  class="flex h-8 w-8 items-center justify-center rounded-full bg-neutral-900 text-white transition hover:bg-neutral-700 disabled:cursor-not-allowed disabled:opacity-40 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-200"
                >
                  <svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 19V5" /><path d="M5 12l7-7 7 7" /></svg>
                </button>
              }
            >
              <button onClick={props.onStop} title="Stop" aria-label="Stop" class="flex h-8 w-8 items-center justify-center rounded-full bg-red-600 text-white transition hover:bg-red-500"><svg viewBox="0 0 24 24" class="h-3.5 w-3.5" fill="currentColor" aria-hidden="true"><rect x="6" y="6" width="12" height="12" rx="1.5" /></svg></button>
            </Show>
          </div>
        </div>
      </div>
    </footer>
  );
}
