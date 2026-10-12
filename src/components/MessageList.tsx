import { For, Index, Show } from "solid-js";
import Markdown from "../lib/Markdown";
import { DocIcon, MarkAvatar } from "../lib/icons";
import { formatBytes, parseAttachments, type Turn } from "../lib/turns";
import { Timeline } from "./Timeline";

/// The per-turn rendering: user bubble, activity timeline mount, answer bubble
/// and any trailing memory note. Bottom-anchored by the parent section.
export function MessageList(props: {
  turns: Turn[];
  empty: boolean;
  thinkingOpenId: string | null;
  onToggleThought: (id: string) => void;
  onApprove: (callId: string) => void;
  onDeny: (callId: string) => void;
}) {
  return (
    <div class="flex min-h-full flex-col justify-end space-y-3">
      <Index each={props.turns}>
        {(turn) => {
          const t = () => turn();
          const answer = () => t().answer;
          const showAnswer = () =>
            !!answer() &&
            (!!answer()!.content.trim() ||
              (t().live && !!answer()!.id.startsWith("tmp-assistant-")));
          return (
            <div class="space-y-3">
              <Show when={t().user}>
                {(u) => (
                  <div class="flex justify-end">
                    <div class="max-w-[80%]">
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
                                    class="max-h-64 max-w-full rounded-lg border border-white/20 dark:border-neutral-900/20"
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
                  </div>
                )}
              </Show>

              <Show when={t().entries.length > 0}>
                <Timeline
                  entries={t().entries}
                  live={t().live}
                  openThoughtId={props.thinkingOpenId}
                  onToggleThought={props.onToggleThought}
                  onApprove={props.onApprove}
                  onDeny={props.onDeny}
                />
              </Show>

              <Show when={showAnswer()}>
                <div class="flex justify-start">
                  <div class="max-w-[80%]">
                    <div class="rounded-xl bg-neutral-100 px-4 py-2.5 text-sm dark:bg-neutral-800">
                      <div class="mb-1 flex items-center gap-1.5 text-[11px]">
                        <MarkAvatar class="h-3.5 w-3.5 shrink-0" />
                        <span class="opacity-60">
                          assistant
                          {answer()!.stop_reason === "aborted" && " (aborted)"}
                          {answer()!.stop_reason === "error" && " (error)"}
                        </span>
                      </div>
                      <Show when={parseAttachments(answer()!).length > 0}>
                        <div class="mb-2 flex flex-wrap items-end gap-2">
                          <For each={parseAttachments(answer()!)}>
                            {(a) =>
                              a.kind === "image" && a.dataUrl ? (
                                <img
                                  src={a.dataUrl}
                                  alt={a.name}
                                  class="max-h-64 max-w-full rounded-lg border border-neutral-300 dark:border-neutral-700"
                                />
                              ) : (
                                <div class="flex items-center gap-2 rounded-lg border border-neutral-300 bg-neutral-100 px-2.5 py-1.5 text-xs dark:border-neutral-700 dark:bg-neutral-800">
                                  <DocIcon />
                                  <span class="max-w-52 truncate">{a.name}</span>
                                  <span class="opacity-60">{formatBytes(a.size)}</span>
                                </div>
                              )
                            }
                          </For>
                        </div>
                      </Show>
                      <Show
                        when={answer()!.content}
                        fallback={
                          <div class="whitespace-pre-wrap break-words">
                            {answer()!.content}
                          </div>
                        }
                      >
                        <Markdown text={answer()!.content} />
                      </Show>
                      {t().live && answer()!.id.startsWith("tmp-assistant-") && (
                        <span class="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-neutral-400 align-text-bottom" />
                      )}
                    </div>
                  </div>
                </div>
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
