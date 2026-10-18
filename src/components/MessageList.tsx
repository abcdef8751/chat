import { For, Index, Show } from "solid-js";
import type { Attachment, Message } from "../lib/api";
import { DocIcon, PaperclipIcon } from "../lib/icons";
import { isAndroid } from "../lib/platform";
import { formatBytes, parseAttachments, type Turn } from "../lib/turns";
import { openImage } from "./ImageLightbox";
import { Timeline } from "./Timeline";

/// The per-turn rendering: user bubble, then the assistant side as one ordered
/// stream of reasoning containers / output segments / tool cards, and any
/// trailing memory note. Bottom-anchored by the parent section.
export function MessageList(props: {
  turns: Turn[];
  empty: boolean;
  streaming: boolean;
  thinkingOpenId: string | null;
  onToggleThought: (id: string, text: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
  editingId: string | null;
  editDraft: string;
  setEditDraft: (value: string) => void;
  editAttachments: Attachment[];
  onStartEdit: (m: Message) => void;
  onCancelEdit: () => void;
  onSaveEdit: () => void;
  onRemoveEditAttachment: (id: string) => void;
  onPickEditAttachments: () => void;
}) {
  /// Expand any image in the transcript (attachment images and images rendered
  /// by markdown prose) in the lightbox. Delegated here so the shared markdown
  /// renderer needs no change; a click on the edit strip's "remove attachment"
  /// button is not an IMG, so it keeps its own handler.
  function onTranscriptClick(e: MouseEvent) {
    const target = e.target as HTMLElement | null;
    if (target?.tagName !== "IMG") return;
    const img = target as HTMLImageElement;
    // An `src=""` attribute resolves to the document URL, so require the real
    // attribute rather than trusting `img.src`.
    if (!img.getAttribute("src")) return;
    const src = img.currentSrc || img.src;
    if (!src) return;
    openImage(src, img.alt || undefined);
  }

  return (
    <div
      onClick={onTranscriptClick}
      class="flex min-h-full flex-col justify-end space-y-3 [&_img]:cursor-zoom-in"
    >
      <Index each={props.turns}>
        {(turn) => {
          const t = () => turn();
          return (
            <div class="space-y-3">
              <Show when={t().user}>
                {(u) => (
                  <div class="flex justify-end">
                    <div class="max-w-[80%]">
                      <Show
                        when={props.editingId === u().id}
                        fallback={
                          <>
                            <div class="flex items-start justify-end gap-1.5">
                              <button
                                onClick={() => props.onStartEdit(u())}
                                disabled={props.streaming}
                                title="Edit this message"
                                aria-label="Edit message"
                                class="mt-1 shrink-0 rounded-md border border-neutral-300 px-2 py-1 text-[11px] text-neutral-500 transition hover:bg-neutral-100 disabled:opacity-40 dark:border-neutral-700 dark:text-neutral-400 dark:hover:bg-neutral-800"
                              >
                                Edit
                              </button>
                              <div class="rounded-xl bg-neutral-900 px-4 py-2.5 text-sm text-white dark:bg-white dark:text-neutral-900">
                                <div class="mb-0.5 text-[11px] opacity-60">you</div>
                                <Show when={parseAttachments(u()).length > 0}>
                                  <div class="mb-2 flex flex-wrap items-end gap-2">
                                    <For each={parseAttachments(u())}>
                                      {(a) =>
                                        a.kind === "image" && a.dataUrl ? (
                                          <img
                                            src={a.dataUrl}
                                            alt={a.name}
                                            class="max-h-64 max-w-full cursor-zoom-in rounded-lg border border-white/20 dark:border-neutral-900/20"
                                          />
                                        ) : (
                                          <div class="flex items-center gap-2 rounded-lg border border-white/25 bg-white/10 px-2.5 py-1.5 text-xs dark:border-neutral-900/20 dark:bg-neutral-900/10">
                                            <DocIcon />
                                            <span class="max-w-52 truncate">{a.name}</span>
                                            <span class="opacity-60">{formatBytes(a.size)}</span>
                                          </div>
                                        )
                                      }
                                    </For>
                                  </div>
                                </Show>
                                <div class="whitespace-pre-wrap break-words">{u().content}</div>
                              </div>
                            </div>
                          </>
                        }
                      >
                        <div class="rounded-xl border border-neutral-300 bg-white p-3 text-sm shadow-sm dark:border-neutral-700 dark:bg-neutral-900">
                          <Show when={props.editAttachments.length > 0}>
                            <div class="mb-2 flex flex-wrap items-end gap-2">
                              <For each={props.editAttachments}>
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
                                      onClick={() => props.onRemoveEditAttachment(a.id)}
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
                          <textarea
                            ref={(el) => el.focus()}
                            class="w-full resize-none rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-900 dark:focus:border-neutral-500"
                            rows={3}
                            value={props.editDraft}
                            onInput={(e) => props.setEditDraft(e.currentTarget.value)}
                            onKeyDown={(e) => {
                              // Escape cancels; Enter saves on desktop, newlines
                              // on Android (matching the composer).
                              if (e.key === "Escape") {
                                e.preventDefault();
                                props.onCancelEdit();
                              } else if (e.key === "Enter" && !e.shiftKey && !isAndroid) {
                                e.preventDefault();
                                props.onSaveEdit();
                              }
                            }}
                          />
                          <div class="mt-2 flex items-center justify-between gap-2">
                            <button
                              onClick={props.onPickEditAttachments}
                              title="Attach files"
                              aria-label="Attach files"
                              class="flex h-9 w-9 items-center justify-center rounded-lg border border-neutral-300 text-neutral-500 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-400 dark:hover:bg-neutral-800"
                            >
                              <PaperclipIcon />
                            </button>
                            <div class="flex gap-2">
                              <button
                                onClick={props.onCancelEdit}
                                class="rounded-lg border border-neutral-300 px-3 py-1.5 text-xs text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800"
                              >
                                Cancel
                              </button>
                              <button
                                onClick={props.onSaveEdit}
                                class="rounded-lg bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                              >
                                Save
                              </button>
                            </div>
                          </div>
                        </div>
                      </Show>
                    </div>
                  </div>
                )}
              </Show>

              <Show when={t().entries.length > 0 || !!t().answer}>
                <Timeline
                  entries={t().entries}
                  answer={t().answer}
                  live={t().live}
                  openThoughtId={props.thinkingOpenId}
                  onOpenThought={props.onToggleThought}
                  onApprove={props.onApprove}
                  onDeny={props.onDeny}
                />
              </Show>

              <Show when={t().note}>
                {(n) => (
                  <div class="flex justify-center">
                    <span class="rounded-full bg-neutral-100 px-3 py-1 text-[11px] text-neutral-500 dark:bg-neutral-900 dark:text-neutral-400">
                      {n().content}
                    </span>
                  </div>
                )}
              </Show>
            </div>
          );
        }}
      </Index>

      <Show when={props.empty}>
        <p class="text-sm text-neutral-400 dark:text-neutral-500">No messages yet.</p>
      </Show>
    </div>
  );
}
