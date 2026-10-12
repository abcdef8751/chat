import { For, Show } from "solid-js";
import type { Attachment } from "../lib/api";
import { DocIcon, PaperclipIcon } from "../lib/icons";
import { formatBytes } from "../lib/turns";

/// The composer: staged attachments, the near-limit / vision warnings, the
/// textarea and the send/stop controls.
export function Composer(props: {
  pendingAttachments: Attachment[];
  onRemoveAttachment: (id: string) => void;
  contextNote: string | null;
  attachmentWarning: string | null;
  streaming: boolean;
  onPickAttachments: () => void;
  draft: string;
  setDraft: (value: string) => void;
  onSend: () => void;
  onStop: () => void;
}) {
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
        <p class="mb-2 rounded-lg border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-700 dark:bg-amber-950/40 dark:text-amber-300">
          {props.attachmentWarning}
        </p>
      </Show>
      <div class="flex items-end gap-2">
        <button
          onClick={() => void props.onPickAttachments()}
          disabled={props.streaming}
          title="Attach files"
          class="flex h-[3rem] w-[3rem] shrink-0 items-center justify-center rounded-xl border border-neutral-300 text-neutral-500 transition hover:bg-neutral-100 disabled:opacity-40 dark:border-neutral-700 dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          <PaperclipIcon />
        </button>
        <textarea
          class="min-h-[3rem] flex-1 resize-none rounded-xl border border-neutral-300 px-4 py-3 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-900 dark:focus:border-neutral-500"
          rows={3}
          placeholder={props.streaming ? "Assistant is replying…" : "Message Pi Chat…"}
          value={props.draft}
          disabled={props.streaming}
          onInput={(e) => props.setDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              props.onSend();
            }
          }}
        />
        <Show
          when={props.streaming}
          fallback={
            <button
              onClick={props.onSend}
              disabled={!props.draft.trim() && props.pendingAttachments.length === 0}
              class="h-[3rem] shrink-0 rounded-xl bg-neutral-900 px-5 text-sm font-medium text-white transition hover:bg-neutral-700 disabled:cursor-not-allowed disabled:opacity-40 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
            >
              Send
            </button>
          }
        >
          <button
            onClick={props.onStop}
            class="h-[3rem] shrink-0 rounded-xl bg-red-600 px-5 text-sm font-medium text-white transition hover:bg-red-500"
          >
            Stop
          </button>
        </Show>
      </div>
    </footer>
  );
}
