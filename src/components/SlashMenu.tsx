import { createEffect, For, Show } from "solid-js";
import type { SlashItem } from "../lib/api";

/// The composer's `/` popup: a positioned list above the chatbox.
///
/// The parent owns filtering and the active index (so the textarea's keyboard
/// handler can drive it); this file only renders. Hover reports back so the
/// pointer and the arrow keys share one selection.
export function SlashMenu(props: {
  items: SlashItem[];
  activeIndex: number;
  onHover: (index: number) => void;
  onSelect: (item: SlashItem) => void;
}) {
  let listEl: HTMLDivElement | undefined;

  // Keep the highlighted row visible in a long skill list.
  createEffect(() => {
    const index = props.activeIndex;
    const rows = listEl?.querySelectorAll<HTMLElement>("[data-slash-row]");
    rows?.[index]?.scrollIntoView({ block: "nearest" });
  });

  return (
    <Show when={props.items.length > 0}>
      <div
        ref={listEl}
        role="listbox"
        aria-label="Slash commands"
        class="absolute inset-x-0 bottom-full z-40 mb-2 max-h-64 overflow-y-auto rounded-xl border border-neutral-200 bg-white p-1 shadow-lg dark:border-neutral-700 dark:bg-neutral-900"
      >
        <For each={props.items}>
          {(item, index) => (
            <button
              type="button"
              data-slash-row
              role="option"
              aria-selected={index() === props.activeIndex}
              // Keep the textarea focused while clicking with a pointer, so
              // selecting a skill leaves the caret ready for its arguments.
              onMouseDown={(e) => e.preventDefault()}
              onMouseEnter={() => props.onHover(index())}
              onClick={() => props.onSelect(item)}
              class={
                "flex w-full items-baseline gap-2 rounded-lg px-2.5 py-1.5 text-left transition " +
                (index() === props.activeIndex
                  ? "bg-neutral-100 text-neutral-900 dark:bg-neutral-800 dark:text-neutral-100"
                  : "text-neutral-700 hover:bg-neutral-50 dark:text-neutral-300 dark:hover:bg-neutral-800/60")
              }
            >
              <span class="shrink-0 font-mono text-xs font-semibold" title={item.name}>
                /{item.id}
              </span>
              <span class="truncate text-[11px] text-neutral-500 dark:text-neutral-400">
                {item.description}
              </span>
              <Show when={item.kind === "command"}>
                <span class="ml-auto shrink-0 rounded-full bg-neutral-200 px-1.5 py-0.5 text-[10px] font-medium text-neutral-600 dark:bg-neutral-700 dark:text-neutral-300">
                  cmd
                </span>
              </Show>
            </button>
          )}
        </For>
      </div>
    </Show>
  );
}
