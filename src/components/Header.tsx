import { createEffect, For, Show } from "solid-js";
import { DropdownMenu } from "@kobalte/core/dropdown-menu";
import type { ModelInfo, Pricing, ProviderInfo, ThinkingOptions } from "../lib/api";
import { MenuIcon, MoreIcon } from "../lib/icons";
import { formatTokens } from "../lib/turns";

// Shared styling for dropdown-menu items (header overflow menu). Kept as a
// literal here so Tailwind's scanner picks up the `data-[highlighted]` variant.
const MENU_ITEM_CLASS =
  "flex cursor-pointer select-none items-center rounded-md px-3 py-2 text-neutral-700 outline-none data-[highlighted]:bg-neutral-100 dark:text-neutral-200 dark:data-[highlighted]:bg-neutral-800";

/// The app header: conversation title, context/cost meter, and the
/// provider/model/thinking pickers (with a narrow-viewport overflow menu).
export function Header(props: {
  sidebarOpen: boolean;
  setSidebarOpen: (open: boolean) => void;
  onHamburgerRef: (el: HTMLButtonElement) => void;
  activeTitle: string;
  activeId: string | null;
  contextTokens: number;
  contextWindow: number;
  pricing: Pricing | null;
  reflectionLabel: string | null;
  compacted: boolean;
  contextClass: string;
  costLabel: string;
  dark: boolean;
  setDark: (value: boolean) => void;
  providers: ProviderInfo[];
  headerProviderId: string;
  changeHeaderProvider: (id: string) => void;
  models: ModelInfo[];
  modelsLoading: boolean;
  model: string;
  changeModel: (id: string) => void;
  modelOptions: () => ModelInfo[];
  thinkingOpts: ThinkingOptions | null;
  thinkingLevel: string;
  changeThinking: (level: string) => void;
  openMemory: () => void;
  openSettings: () => void;
}) {
  // Re-apply the selection after the option list is rebuilt. WebKit clears a
  // <select> when its selected <option> is removed during a refresh, and the
  // `value` binding alone won't restore it because `model()` is unchanged.
  let modelSelectEl: HTMLSelectElement | undefined;
  createEffect(() => {
    // With no provider the model selector is blank, so the value is "" (the
    // "No provider" placeholder) regardless of whatever `model()` still holds.
    const value = props.providers.length === 0 ? "" : props.model;
    props.models;
    if (modelSelectEl && modelSelectEl.value !== value) {
      modelSelectEl.value = value;
    }
  });

  // Same WebKit quirk for the provider select: when the neighbor model select
  // rebuilds its options (e.g. the new provider's model list lands), WebKit
  // resets this select to its first option — which is the active/old provider —
  // and the `value` binding won't restore it because `headerProviderId()` is
  // unchanged. Force the DOM value back to the selected provider whenever the
  // provider selection, the option list, or the model-list (neighbor) rebuild
  // changes.
  let providerSelectEl: HTMLSelectElement | undefined;
  createEffect(() => {
    const value = props.headerProviderId;
    props.providers;
    props.models;
    props.modelsLoading;
    if (providerSelectEl && providerSelectEl.value !== value) {
      providerSelectEl.value = value;
    }
  });

  return (
    <header class="flex items-center justify-between gap-3 border-b border-neutral-200 px-4 py-3 sm:px-6 dark:border-neutral-800">
      <div class="flex min-w-0 items-center gap-2">
        <button
          ref={(el) => props.onHamburgerRef(el)}
          onClick={() => props.setSidebarOpen(true)}
          title="Show chats"
          aria-label="Show chats"
          aria-controls="app-sidebar"
          aria-expanded={props.sidebarOpen}
          class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          <MenuIcon />
        </button>
        <h1 class="truncate text-sm font-semibold">{props.activeTitle}</h1>
      </div>
      <div class="flex min-w-0 flex-wrap items-center justify-end gap-2 sm:gap-3">
        <Show when={props.activeId}>
          <div
            class="hidden items-center gap-1.5 rounded-md px-2 py-1 text-[11px] text-neutral-500 sm:flex dark:text-neutral-400"
            title={`Context: ${Math.min(props.contextTokens, props.contextWindow).toLocaleString()} / ${props.contextWindow.toLocaleString()} tokens\nInput / output: ${
              props.pricing?.inputPerMillion ?? "—"
            } / ${props.pricing?.outputPerMillion ?? "—"} USD per 1M\nCache read / write: ${
              props.pricing?.cacheReadPerMillion ?? "—"
            } / ${props.pricing?.cacheWritePerMillion ?? "—"} USD per 1M${
              props.pricing?.overridden ? "\n(using your overrides)" : ""
            }${props.reflectionLabel ? `\nBackground maintenance: ${props.reflectionLabel}` : ""}${
              props.compacted ? "\nOlder messages are compacted into a summary." : ""
            }`}
          >
            <span class={props.contextClass}>
              {formatTokens(Math.min(props.contextTokens, props.contextWindow))} /{" "}
              {formatTokens(props.contextWindow)} tok
            </span>
            <span class="text-neutral-300 dark:text-neutral-600">·</span>
            <span>{props.costLabel}</span>
            <Show when={props.compacted}>
              <span class="rounded bg-neutral-100 px-1.5 py-0.5 text-[10px] text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400">
                compacted
              </span>
            </Show>
          </div>
        </Show>
        <button
          onClick={() => props.setDark(!props.dark)}
          title="Toggle light/dark theme"
          aria-pressed={props.dark}
          class="hidden rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 lg:inline-block dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          {props.dark ? "Light" : "Dark"}
        </button>
        <Show when={props.providers.length > 0 && props.activeId}>
          <select
            ref={(el) => (providerSelectEl = el)}
            class="hidden max-w-28 truncate rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 disabled:opacity-50 sm:block md:max-w-36 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
            title="Provider"
            value={props.headerProviderId}
            onChange={(e) => void props.changeHeaderProvider(e.currentTarget.value)}
          >
            <For each={props.providers}>
              {(p) => (
                <option value={p.id}>
                  {p.name}
                  {!p.hasKey ? " (no key)" : ""}
                </option>
              )}
            </For>
          </select>
        </Show>
        <div>
          <select
            class="max-w-[9rem] pr-7 truncate rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 disabled:opacity-60 sm:max-w-56 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
            title="Model"
            aria-busy={props.modelsLoading}
            disabled={props.modelsLoading || props.providers.length === 0}
            ref={(el) => (modelSelectEl = el)}
            value={props.providers.length === 0 ? "" : props.model}
            onChange={(e) => {
              const next = e.currentTarget.value;
              // Ignore spurious events from the list refreshing, only
              // persist a real user selection.
              if (next && next !== props.model) void props.changeModel(next);
            }}
          >
            <Show when={props.providers.length === 0}>
              <option value="" disabled>
                No provider
              </option>
            </Show>
            <For each={props.modelOptions()}>
              {(m) => <option value={m.id}>{m.name?.trim() || m.id}</option>}
            </For>
          </select>
        </div>
        {/* Spinner rendered as an inline sibling after the select (not
            overlaid inside its box). Small + inline so it doesn't clip or
            push neighbor controls on narrow windows. */}
        <Show when={props.modelsLoading}>
          <span
            role="status"
            aria-label="Loading models"
            title="Loading models…"
            class="ml-1 inline-block h-3.5 w-3.5 shrink-0 animate-spin rounded-full border-2 border-neutral-400 border-t-transparent"
          />
        </Show>
        <Show
          when={props.thinkingOpts?.supportsReasoning && props.thinkingOpts.options.length > 0}
        >
          <select
            class="hidden rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 lg:block dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200 dark:focus:border-neutral-500"
            title={
              props.thinkingOpts!.source === "models.dev"
                ? "Thinking level (from models.dev)"
                : "Thinking level (OpenAI reasoning_effort)"
            }
            value={props.thinkingLevel}
            onChange={(e) => props.changeThinking(e.currentTarget.value)}
          >
            <option value="">Thinking: default</option>
            <For each={props.thinkingOpts!.options}>
              {(o) => <option value={o}>Thinking: {o}</option>}
            </For>
          </select>
        </Show>
        <button
          onClick={props.openMemory}
          class="hidden rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 lg:inline-block dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          Memory
        </button>
        <button
          onClick={props.openSettings}
          class="hidden rounded-md px-3 py-1.5 text-xs text-neutral-500 transition hover:bg-neutral-100 lg:inline-block dark:text-neutral-400 dark:hover:bg-neutral-800"
        >
          Settings
        </button>
        {/* Overflow menu: the header controls above collapse into this below
            `lg`, so nothing becomes unreachable when the window is narrow. */}
        <DropdownMenu>
          <DropdownMenu.Trigger
            title="More actions"
            aria-label="More actions"
            class="rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 data-[expanded]:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800 dark:data-[expanded]:bg-neutral-800"
          >
            <MoreIcon />
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content
              class="z-50 min-w-44 rounded-lg border border-neutral-200 bg-white p-1 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
            >
              <DropdownMenu.Item class={MENU_ITEM_CLASS} onSelect={props.openMemory}>
                Memory
              </DropdownMenu.Item>
              <DropdownMenu.Item class={MENU_ITEM_CLASS} onSelect={props.openSettings}>
                Settings
              </DropdownMenu.Item>
              <DropdownMenu.Item
                class={MENU_ITEM_CLASS}
                onSelect={() => props.setDark(!props.dark)}
              >
                {props.dark ? "Switch to light" : "Switch to dark"}
              </DropdownMenu.Item>
              <Show
                when={
                  props.thinkingOpts?.supportsReasoning && props.thinkingOpts.options.length > 0
                }
              >
                <DropdownMenu.Separator class="my-1 h-px bg-neutral-200 dark:bg-neutral-800" />
                <DropdownMenu.Sub>
                  <DropdownMenu.SubTrigger
                    class={`${MENU_ITEM_CLASS} justify-between gap-3`}
                  >
                    <span>Thinking: {props.thinkingLevel || "default"}</span>
                    <span class="text-neutral-400">›</span>
                  </DropdownMenu.SubTrigger>
                  <DropdownMenu.Portal>
                    <DropdownMenu.SubContent
                      class="z-50 min-w-40 rounded-lg border border-neutral-200 bg-white p-1 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
                    >
                      <DropdownMenu.RadioGroup
                        value={props.thinkingLevel}
                        onChange={(value) => props.changeThinking(value)}
                      >
                        <DropdownMenu.RadioItem value="" class={MENU_ITEM_CLASS}>
                          <span class="inline-block w-4">
                            {props.thinkingLevel === "" ? "✓" : ""}
                          </span>
                          Default
                        </DropdownMenu.RadioItem>
                        <For each={props.thinkingOpts!.options}>
                          {(o) => (
                            <DropdownMenu.RadioItem value={o} class={MENU_ITEM_CLASS}>
                              <span class="inline-block w-4">
                                {props.thinkingLevel === o ? "✓" : ""}
                              </span>
                              Thinking: {o}
                            </DropdownMenu.RadioItem>
                          )}
                        </For>
                      </DropdownMenu.RadioGroup>
                    </DropdownMenu.SubContent>
                  </DropdownMenu.Portal>
                </DropdownMenu.Sub>
              </Show>
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu>
      </div>
    </header>
  );
}
