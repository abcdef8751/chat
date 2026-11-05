import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import type { Attachment, SlashItem } from "../lib/api";
import { DocIcon } from "../lib/icons";
import { isAndroid } from "../lib/platform";
import { formatBytes } from "../lib/turns";
import { AttachmentSheet } from "./AttachmentSheet";
import { SlashMenu } from "./SlashMenu";

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
  /// Ask the platform for a specific kind of attachment (Android). When absent
  /// the sheet falls back to `onPickAttachments` for every action, which is the
  /// desktop behaviour.
  onPickKind?: (kind: "any" | "image" | "camera") => void;
  /// Offer the Camera action in the attachment sheet.
  canCamera?: boolean;
  /// The effective auto-approve setting for this chat (the global default when
  /// the chat inherits it, otherwise the chat's override).
  yolo?: boolean;
  /// True when the chat inherits the Settings value instead of overriding it.
  yoloInherited?: boolean;
  /// Flip this chat's auto-approve choice (the app owns the persistence).
  onToggleYolo?: () => void;
  /// Clear this chat's explicit choice (back to the Settings default).
  onClearYolo?: () => void;
  /// Rows for the `/` menu, supplied by the app (skills plus its own commands
  /// such as `compact`). The composer never invents entries: no matches means
  /// no menu.
  slashItems?: SlashItem[];
  /// Run the app's `/compact` action.
  onRunCompact?: () => void;
  /// Retry a failed remote upload for a staged attachment (shown as a warning
  /// next to the file name when it has nothing inline and no remote path).
  onRetryAttachment?: (attachment: Attachment) => void;
  draft: string;
  setDraft: (value: string) => void;
  onSend: () => void;
  onStop: () => void;
  /// The user is editing an existing message: the composer holds its text and
  /// attachments and sending rewrites that row instead of appending a new turn.
  editing?: boolean;
  onCancelEdit?: () => void;
  /// Bumped by the parent when the composer should take focus (e.g. clicking a
  /// message's edit pen loads it here); the value itself is meaningless.
  focusNonce?: number;
}) {
  let textareaEl: HTMLTextAreaElement | undefined;
  const busy = () => props.busy ?? props.streaming;

  const [sheetOpen, setSheetOpen] = createSignal(false);
  const [slashActive, setSlashActive] = createSignal(0);
  // Escape hides the `/` menu without rewriting the draft; any edit re-arms
  // it. A bare query matches again after Escape, so this is needed.
  const [slashDismissed, setSlashDismissed] = createSignal(false);
  // A destructive/built-in command (`/compact`) must require an explicit
  // up/down/hover selection. Enter on a fresh `/` must never compact by
  // accident.
  const [slashNavigated, setSlashNavigated] = createSignal(false);

  // Only a bare `/token` (optional leading whitespace, no whitespace after the
  // slash) opens the menu; once the user types a space they are writing the
  // message, not selecting an item.
  const slashQuery = createMemo(() => {
    const match = /^\s*\/([^\s]*)$/.exec(props.draft);
    return match ? match[1] : null;
  });

  const slashResults = createMemo(() => {
    const query = slashQuery();
    if (query === null) return [];
    const needle = query.toLowerCase();
    return (props.slashItems ?? []).filter(
      (item) =>
        item.id.toLowerCase().includes(needle) ||
        item.name.toLowerCase().includes(needle) ||
        item.description.toLowerCase().includes(needle),
    );
  });

  const slashOpen = () => !slashDismissed() && slashResults().length > 0;

  // A changed list (or a closed menu) starts the selection over at the top.
  createEffect(() => {
    slashResults();
    slashOpen();
    setSlashActive(0);
    setSlashNavigated(false);
  });

  // The slash menu and the attachment sheet share the same anchor: opening one
  // closes the other.
  createEffect(() => {
    if (slashOpen()) setSheetOpen(false);
  });

  const chooseSlash = (item: SlashItem) => {
    if (item.kind === "skill") {
      // Insert the FILE STEM (the backend resolves `/` tokens by id), not the
      // human display name; keep the caret in the textarea for details.
      props.setDraft(`/${item.id} `);
      queueMicrotask(() => {
        textareaEl?.focus();
        autoResize();
      });
      return;
    }
    if (item.kind === "command" && item.id === "compact") {
      setSlashDismissed(true);
      props.onRunCompact?.();
      props.setDraft("");
      return;
    }
    // Unknown command: dismiss rather than inserting a token nothing handles.
    setSlashDismissed(true);
  };

  const pickAttachment = (kind: "any" | "image" | "camera") => {
    if (props.onPickKind) {
      props.onPickKind(kind);
      return;
    }
    // Desktop (and any host without kind-aware picking) opens the ordinary
    // attachment picker. `File` is the documented fallback; honouring it for
    // Camera/Photo keeps the sheet useful there too.
    props.onPickAttachments();
  };

  // The sheet is a popover, not a modal: a busy composer has no business
  // leaving it open over a disabled chatbox.
  createEffect(() => {
    if (busy()) setSheetOpen(false);
  });

  // Return focus to the composer once a turn finishes (including after a
  // Stop), so the chatbox is immediately usable again. Skip on Android:
  // focusing there pops the on-screen keyboard after the agent finishes
  // typing (issue 6).
  let prevStreaming = props.streaming;
  createEffect(() => {
    const s = props.streaming;
    if (prevStreaming && !s && !isAndroid && !busy()) textareaEl?.focus();
    prevStreaming = s;
  });

  const autoResize = () => {
    const el = textareaEl;
    if (!el) return;
    el.style.height = "auto";
    // Use the *visual* viewport when the platform exposes it: on mobile the
    // on-screen keyboard shrinks it, and sizing against the full window would
    // let the textarea grow behind the keyboard. The intent stays 60% / ~14
    // lines, then scroll.
    const viewportHeight =
      typeof window !== "undefined"
        ? (window.visualViewport?.height ?? window.innerHeight)
        : 360;
    const cap = viewportHeight * 0.6;
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

  // Focus on demand: clicking a message's edit pen loads that text into this
  // composer, and the user should be able to type/Enter straight away. Android
  // is skipped on purpose — focusing raises the soft keyboard, and the pen tap
  // may be followed by the user scrolling the transcript instead.
  let lastFocusNonce = props.focusNonce ?? 0;
  createEffect(() => {
    const nonce = props.focusNonce ?? 0;
    if (nonce === lastFocusNonce) return;
    lastFocusNonce = nonce;
    if (!isAndroid) queueMicrotask(() => textareaEl?.focus());
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
                      {/* Nothing inline to send: either it is stored remotely and
                          the model fetches it through a tool, or the upload
                          failed and the user can retry from here. */}
                      <Show when={a.remotePath && !a.text && !a.dataUrl}>
                        <span
                          title="Stored remotely — the assistant downloads it on demand"
                          class="shrink-0 rounded-full bg-neutral-200 px-1.5 py-0.5 text-[10px] font-medium text-neutral-600 dark:bg-neutral-700 dark:text-neutral-300"
                        >
                          remote
                        </span>
                      </Show>
                      <Show when={!a.remotePath && !a.text && !a.dataUrl}>
                        <Show
                          when={a.sourcePath}
                          fallback={
                            <span
                              title={a.uploadError ?? "This file could not be uploaded"}
                              class="shrink-0 rounded-full bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-800 dark:bg-amber-950/50 dark:text-amber-300"
                            >
                              ⚠ not uploaded
                            </span>
                          }
                        >
                          <button
                            type="button"
                            onClick={() => props.onRetryAttachment?.(a)}
                            title={
                              a.uploadError
                                ? `Upload failed: ${a.uploadError} — click to retry`
                                : "Not uploaded — click to retry"
                            }
                            class="shrink-0 rounded-full border border-amber-400/70 bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-800 transition hover:bg-amber-200 dark:border-amber-700 dark:bg-amber-950/50 dark:text-amber-300 dark:hover:bg-amber-900/60"
                          >
                            ⚠ retry
                          </button>
                        </Show>
                      </Show>
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
      <Show when={props.editing}>
        <div class="mb-2 flex items-center justify-between gap-2 rounded-lg border border-amber-300 bg-amber-50 px-3 py-1.5 text-xs text-amber-800 dark:border-amber-700 dark:bg-amber-950/40 dark:text-amber-300">
          <span class="flex min-w-0 items-center gap-1.5">
            <span class="font-medium">Editing message</span>
            <span class="truncate opacity-70">— sending replaces it and re-runs the reply</span>
          </span>
          <button
            type="button"
            onClick={() => props.onCancelEdit?.()}
            class="shrink-0 rounded-md border border-amber-400/60 px-2 py-0.5 text-[11px] font-medium transition hover:bg-amber-100 dark:border-amber-700 dark:hover:bg-amber-900/40"
          >
            Cancel
          </button>
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
      <div class="relative">
        <Show when={slashOpen()}>
          <SlashMenu
            items={slashResults()}
            activeIndex={slashActive()}
            onHover={(index) => setSlashActive(index)}
            onSelect={chooseSlash}
          />
        </Show>
        <AttachmentSheet
          open={sheetOpen()}
          canCamera={props.canCamera}
          yolo={props.yolo}
          yoloInherited={props.yoloInherited}
          onPick={pickAttachment}
          onToggleYolo={props.onToggleYolo}
          onClearYolo={props.onClearYolo}
          onClose={() => setSheetOpen(false)}
        />
        <div class="flex flex-col rounded-[28px] border border-neutral-300 bg-white p-3 shadow-sm focus-within:border-neutral-400 dark:border-neutral-700 dark:bg-neutral-900 dark:focus-within:border-neutral-600">
        <textarea
          ref={(el) => {
            textareaEl = el;
            queueMicrotask(autoResize);
          }}
          class="max-h-[60vh] min-h-[3rem] w-full resize-none bg-transparent px-2 py-1 text-sm leading-6 outline-none placeholder:text-neutral-400 dark:text-neutral-100 dark:placeholder:text-neutral-500"
          rows={1}
          placeholder={
            props.editing
              ? "Edit your message…"
              : props.streaming
                ? "Assistant is replying…"
                : "Message Pi Chat…"
          }
          value={props.draft}
          disabled={busy()}
          onFocus={() => {
            // A raised on-screen keyboard can leave the textarea behind it.
            // Ask the layout to make it visible on the next frame; App owns
            // the actual lift/resize.
            const el = textareaEl;
            if (!el) return;
            requestAnimationFrame(() => el.scrollIntoView({ block: "nearest" }));
          }}
          onInput={(e) => {
            setSlashDismissed(false);
            props.setDraft(e.currentTarget.value);
            autoResize();
          }}
          onKeyDown={(e) => {
            // The `/` menu owns the arrow keys and Enter/Tab while it is open,
            // so Enter selects instead of sending and a Tab never moves focus.
            if (slashOpen()) {
              const items = slashResults();
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setSlashNavigated(true);
                setSlashActive((i) => (i + 1) % items.length);
                return;
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setSlashNavigated(true);
                setSlashActive((i) => (i - 1 + items.length) % items.length);
                return;
              }
              if (e.key === "Enter" || e.key === "Tab") {
                const item = items[Math.min(slashActive(), items.length - 1)];
                // Commands have side effects (compaction spends tokens and
                // rewrites the context view). A bare `/` + Enter must not run
                // one: accept an explicit arrow selection OR typing the exact
                // command id (`/compact` + Enter), which is the discoverable
                // path.
                const typedExact =
                  item.kind === "command" &&
                  (slashQuery() ?? "").toLowerCase() === item.id.toLowerCase();
                if (item.kind === "command" && !slashNavigated() && !typedExact) {
                  // No explicit choice: dismiss and let the key do its normal
                  // job (Enter sends; Tab moves focus).
                  setSlashDismissed(true);
                  if (e.key === "Tab") return;
                } else {
                  e.preventDefault();
                  chooseSlash(item);
                  return;
                }
              }
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                setSlashDismissed(true);
                return;
              }
            }
            if (e.key === "Escape" && props.editing) {
              // Escape leaves edit mode from the keyboard. The pen's Cancel
              // button does the same thing; cancelling restores the draft the
              // composer held before the edit loaded.
              e.preventDefault();
              props.onCancelEdit?.();
              return;
            }
            if (e.key === "Enter" && !e.shiftKey && !isAndroid) {
              e.preventDefault();
              handleSend();
            }
          }}
        />
        <div class="mt-2 flex items-center justify-between gap-2">
          <button
            onClick={() => {
              setSlashDismissed(true);
              setSheetOpen((open) => !open);
            }}
            disabled={busy()}
            title="Attach"
            aria-label="Attach"
            aria-haspopup="menu"
            aria-expanded={sheetOpen()}
            class={
              "flex h-8 w-8 shrink-0 items-center justify-center rounded-full border transition disabled:opacity-40 " +
              (sheetOpen()
                ? "border-neutral-400 bg-neutral-200 text-neutral-900 dark:border-neutral-500 dark:bg-neutral-700 dark:text-neutral-100"
                : "border-neutral-200 bg-neutral-50 text-neutral-600 hover:bg-neutral-100 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-300 dark:hover:bg-neutral-700")
            }
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
                  title={props.editing ? "Save & re-run" : "Send"}
                  aria-label={props.editing ? "Save and re-run" : "Send"}
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
      </div>
    </footer>
  );
}
