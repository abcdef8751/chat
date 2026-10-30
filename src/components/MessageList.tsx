import { For, Index, Show } from "solid-js";
import type { Message } from "../lib/api";
import { ClockIcon, DocIcon, EditIcon } from "../lib/icons";
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
  /// Load a persisted user message into the central composer for editing.
  /// Editing no longer happens inline: the composer owns the text, the
  /// attachments and the send/Cancel affordances.
  onStartEdit: (m: Message) => void;
  /// The id of the user row currently loaded into the composer, if any. Drives
  /// the pen's active state so the affordance shows WHICH message is being
  /// edited (and gives the GUI e2e a stable `aria-pressed` hook).
  editingId?: string | null;
  /// messageId → cron job title: a scheduled run posted that user message.
  /// Rendered as a small pill beside the bubble; purely informational.
  cronTriggers?: Record<string, string>;
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
                      <div class="flex items-start justify-end gap-1.5">
                        {/* A scheduled run posted this message: pill with the
                            job's title, truncating long titles. The `title`
                            attribute explains the origin. */}
                        <Show when={props.cronTriggers?.[u().id]}>
                          {(cronTitle) => (
                            <span
                              title={`Posted by the scheduled job “${cronTitle()}”`}
                              class="mt-1 inline-flex max-w-40 items-center gap-1 truncate rounded-full border border-neutral-200 px-2.5 py-1 text-[10px] text-neutral-500 dark:border-neutral-700 dark:text-neutral-400"
                            >
                              <ClockIcon class="h-3 w-3 shrink-0" />
                              <span class="truncate">scheduled · {cronTitle()}</span>
                            </span>
                          )}
                        </Show>
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
                                      {/* Nothing inline: the file lives remotely
                                          and the model fetches it on demand. */}
                                      <Show when={a.remotePath && !a.text && !a.dataUrl}>
                                        <span
                                          title="Stored remotely — the assistant downloads it on demand"
                                          class="shrink-0 rounded-full bg-white/20 px-1.5 py-0.5 text-[10px] font-medium dark:bg-neutral-900/20"
                                        >
                                          remote
                                        </span>
                                      </Show>
                                    </div>
                                  )
                                }
                              </For>
                            </div>
                          </Show>
                          <div class="whitespace-pre-wrap break-words">{u().content}</div>
                        </div>
                      </div>
                      {/* Edit affordance: a small pen BELOW the bubble rather
                          than a full button beside it. Clicking loads the
                          message into the central composer. */}
                      <div class="mt-1 flex justify-end">
                        <button
                          type="button"
                          onClick={() => props.onStartEdit(u())}
                          disabled={props.streaming}
                          title="Edit"
                          aria-label="Edit message"
                          aria-pressed={props.editingId === u().id}
                          class={`rounded-md p-1 transition hover:bg-neutral-100 hover:text-neutral-700 disabled:cursor-not-allowed disabled:opacity-30 dark:hover:bg-neutral-800 dark:hover:text-neutral-300 ${
                            props.editingId === u().id
                              ? "bg-amber-100 text-amber-700 opacity-100 dark:bg-amber-950/60 dark:text-amber-300"
                              : "text-neutral-400 opacity-70 dark:text-neutral-500"
                          }`}
                        >
                          <EditIcon class="h-3.5 w-3.5" />
                        </button>
                      </div>
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
