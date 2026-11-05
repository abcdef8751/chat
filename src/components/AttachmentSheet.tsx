import { createEffect, onCleanup, Show } from "solid-js";
import type { JSX } from "solid-js";

/// The inline menu opened by the composer's `+` button.
///
/// Deliberately NOT a Kobalte `Dialog`: it is an inline popover anchored above
/// the chatbox, rendered inside the footer. The transparent full-screen
/// backdrop is a sibling of the panel, so a click anywhere else closes it while
/// the panel itself stays inside the footer's layout and cannot escape the
/// viewport.
///
/// `onPick` is only called after the sheet closes, so the native picker never
/// opens underneath a still-mounted popover.
export function AttachmentSheet(props: {
  open: boolean;
  canCamera?: boolean;
  yolo?: boolean;
  yoloInherited?: boolean;
  onPick: (kind: "any" | "image" | "camera") => void;
  onToggleYolo?: () => void;
  onClearYolo?: () => void;
  onClose: () => void;
}) {
  // Escape closes the popover even while the textarea/panel has focus. Capture
  // phase so the key is handled here before the composer's own Escape (leave
  // edit mode / close the slash menu).
  createEffect(() => {
    if (!props.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const choose = (kind: "any" | "image" | "camera") => {
    props.onClose();
    props.onPick(kind);
  };

  return (
    <Show when={props.open}>
      <button
        type="button"
        aria-label="Close attachment menu"
        title="Close"
        onClick={() => props.onClose()}
        class="fixed inset-0 z-30 cursor-default bg-transparent"
      />
      <div
        role="dialog"
        aria-label="Attach"
        class="absolute bottom-full left-0 z-40 mb-2 w-[min(20rem,calc(100vw-2rem))] rounded-xl border border-neutral-200 bg-white p-2 shadow-xl dark:border-neutral-700 dark:bg-neutral-900"
      >
        <div class={props.canCamera ? "grid grid-cols-3 gap-1" : "grid grid-cols-2 gap-1"}>
          <Show when={props.canCamera}>
            <Action label="Camera" onClick={() => choose("camera")}>
              <svg viewBox="0 0 24 24" class="h-6 w-6" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" />
                <circle cx="12" cy="13" r="4" />
              </svg>
            </Action>
          </Show>
          <Action label="Photo" onClick={() => choose("image")}>
            <svg viewBox="0 0 24 24" class="h-6 w-6" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <rect x="3" y="3" width="18" height="18" rx="2" />
              <circle cx="8.5" cy="8.5" r="1.5" />
              <path d="M21 15l-5-5L5 21" />
            </svg>
          </Action>
          <Action label="File" onClick={() => choose("any")}>
            <svg viewBox="0 0 24 24" class="h-6 w-6" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
              <path d="M14 2v6h6" />
            </svg>
          </Action>
        </div>

        <div class="my-2 h-px bg-neutral-200 dark:bg-neutral-700" />

        <div class="flex items-center justify-between gap-3 rounded-lg px-2 py-1.5">
          <div class="min-w-0">
            <p class="text-xs font-medium text-neutral-800 dark:text-neutral-200">
              Auto-approve tools · this chat
            </p>
            <p class="text-[11px] text-neutral-500 dark:text-neutral-400">
              {props.yoloInherited ? (
                "Inherited from Settings"
              ) : (
                <button
                  type="button"
                  onClick={() => props.onClearYolo?.()}
                  class="underline decoration-dotted underline-offset-2 transition hover:text-neutral-800 dark:hover:text-neutral-200"
                >
                  Overridden — use default
                </button>
              )}
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={props.yolo ? "true" : "false"}
            aria-label="Auto-approve tools for this chat"
            onClick={() => props.onToggleYolo?.()}
            disabled={!props.onToggleYolo}
            class={
              "relative h-6 w-11 shrink-0 rounded-full transition " +
              (props.yolo ? "bg-neutral-900 dark:bg-white" : "bg-neutral-300 dark:bg-neutral-600") +
              (props.onToggleYolo ? "" : " cursor-not-allowed opacity-40")
            }
          >
            <span
              class={
                "absolute top-0.5 left-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform dark:bg-neutral-900 " +
                (props.yolo ? "translate-x-5" : "translate-x-0")
              }
            />
          </button>
        </div>
      </div>
    </Show>
  );
}

function Action(props: { label: string; onClick: () => void; children: JSX.Element }) {
  return (
    <button
      type="button"
      onClick={() => props.onClick()}
      class="flex flex-col items-center gap-1.5 rounded-xl px-2 py-3 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
    >
      {props.children}
      <span>{props.label}</span>
    </button>
  );
}
