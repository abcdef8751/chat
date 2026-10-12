import { For, Show, type JSX } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import type {
  ModelInfo,
  ModelOverride,
  ModelsDevProvider,
  Pricing,
  ProviderInfo,
  SyncStatus,
} from "../lib/api";
import { BackArrowIcon } from "../lib/icons";
import { formatTokens } from "../lib/turns";

/// The Settings dialog: provider manager, model pricing/context overrides,
/// Brave key, preferences, reflection + shell options, and cloud sync.
export function SettingsDialog(props: {
  settingsOpen: boolean;
  setSettingsOpen: (open: boolean) => void;
  closeSettings: () => void;
  wideViewport: boolean;
  fullscreenInsetStyle: () => JSX.CSSProperties;
  providers: ProviderInfo[];
  providersLoading: boolean;
  providerError: string | null;
  setProviderError: (value: string | null) => void;
  addOpen: boolean;
  setAddOpen: (open: boolean) => void;
  addName: string;
  setAddName: (value: string) => void;
  addUrl: string;
  setAddUrl: (value: string) => void;
  addKey: string;
  setAddKey: (value: string) => void;
  addPick: string;
  setAddPick: (value: string) => void;
  modelsDevProviders: ModelsDevProvider[];
  doAddProvider: () => void;
  editId: string | null;
  setEditId: (id: string | null) => void;
  editName: string;
  setEditName: (value: string) => void;
  editUrl: string;
  setEditUrl: (value: string) => void;
  editKey: string;
  setEditKey: (value: string) => void;
  startEdit: (p: ProviderInfo) => void;
  cancelEdit: () => void;
  doUpdateProvider: () => void;
  doClearProviderKey: (id: string) => void;
  doRemoveProvider: (id: string) => void;
  doSetActive: (id: string) => void;
  modelOptions: () => ModelInfo[];
  overrideModel: string;
  setOverrideModel: (value: string) => void;
  overrideFor: (id: string) => ModelOverride;
  setOverrideField: (id: string, field: keyof ModelOverride, value: number | null) => void;
  overrideEffective: Pricing | null;
  modelOverrides: Record<string, ModelOverride>;
  clearOverride: (id: string) => void;
  numFromInput: (raw: string) => number | null;
  braveKeyDraft: string;
  setBraveKeyDraft: (value: string) => void;
  hasBraveKeySaved: boolean;
  setHasBraveKeySaved: (value: boolean) => void;
  setBraveKey: (braveKey: string | null) => Promise<void>;
  preferences: string;
  setPreferences: (value: string) => void;
  echoReasoning: boolean;
  setEchoReasoning: (value: boolean) => void;
  /**
   * "YOLO mode": auto-approve every gated tool call. Declared here as the frozen
   * Phase-1 contract — App.json plumbed by the app shell; the toggle UI is added
   * with the other Settings work.
   */
  autoApproveTools: boolean;
  setAutoApproveTools: (value: boolean) => void;
  reflectionEnabled: boolean;
  setReflectionEnabled: (value: boolean) => void;
  reflectionIdleMinutes: number;
  setReflectionIdleMinutes: (value: number) => void;
  shellWorkspaceDir: string;
  setShellWorkspaceDir: (value: string) => void;
  syncEnabled: boolean;
  syncBusy: boolean;
  toggleSync: (on: boolean) => void;
  syncStatusData: SyncStatus | null;
  syncAuthMode: "signin" | "signup";
  setSyncAuthMode: (mode: "signin" | "signup") => void;
  syncEmail: string;
  setSyncEmail: (value: string) => void;
  syncPassword: string;
  setSyncPassword: (value: string) => void;
  syncError: string | null;
  setSyncError: (value: string | null) => void;
  syncNotice: string | null;
  setSyncNotice: (value: string | null) => void;
  submitSyncSignIn: () => void;
  submitSyncSignUp: () => void;
  doSyncSignOut: () => void;
  doSyncNow: () => void;
  syncEncBusy: boolean;
  syncRecoveryInput: string;
  setSyncRecoveryInput: (value: string) => void;
  syncRecoveryShown: string | null;
  enableEncryption: () => void;
  unlockEncryption: () => void;
  showRecoveryCode: () => void;
  disableEncryption: () => void;
  syncLabel: (d: SyncStatus | null) => string;
  syncBarWidth: (d: SyncStatus | null) => number;
  formatLastSync: (ms: number | null) => string;
  settingsError: string | null;
  setSettingsError: (value: string | null) => void;
}) {
  return (
    <Dialog
      open={props.settingsOpen}
      onOpenChange={(open) => {
        if (open) props.setSettingsOpen(true);
        else void props.closeSettings();
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
        <Dialog.Content
          class={
            // On narrow (mobile) viewports — the same `lg` threshold that turns
            // the chat list into a drawer — expand to cover the whole viewport
            // so these pages don't render as a cramped centered card.
            props.wideViewport
              ? "fixed left-1/2 top-1/2 z-50 max-h-[85vh] w-full max-w-md -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
              : "fixed inset-0 z-50 h-full w-full overflow-y-auto bg-white p-5 focus:outline-none dark:bg-neutral-900"
          }
          style={props.fullscreenInsetStyle()}
        >
          <div class="flex items-center gap-2">
            {/* Mobile (full-screen) exit arrow — hidden on wide viewports, where
                the centered card keeps its Done button. */}
            <Dialog.CloseButton
              title="Close settings"
              aria-label="Close settings"
              class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              <BackArrowIcon />
            </Dialog.CloseButton>
            <Dialog.Title class="text-base font-semibold">Settings</Dialog.Title>
          </div>
          <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
            Add an OpenAI-compatible endpoint to start chatting.
          </Dialog.Description>

          <div class="mt-4 space-y-3">
            <div>
                <div class="mb-2 flex items-center justify-between">
                  <span class="text-xs font-semibold text-neutral-600 dark:text-neutral-300">
                    Providers
                  </span>
                  <button
                    onClick={() => {
                      props.setAddOpen(!props.addOpen);
                      props.setEditId(null);
                      props.setProviderError(null);
                    }}
                    class="rounded-md bg-neutral-900 px-2 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                  >
                    {props.addOpen ? "Cancel" : "+ Add provider"}
                  </button>
                </div>

                <div class="space-y-2">
                  <Show when={props.providersLoading}>
                    <p class="text-xs text-neutral-400 dark:text-neutral-500">
                      Loading providers…
                    </p>
                  </Show>
                  <Show when={!props.providersLoading && props.providers.length === 0}>
                    <p class="text-xs text-neutral-400 dark:text-neutral-500">
                      No providers yet — add one to start chatting.
                    </p>
                  </Show>
                  <For each={props.providers}>
                    {(p) => (
                      <div class="rounded-lg border border-neutral-200 p-2 dark:border-neutral-700">
                        <Show
                          when={props.editId === p.id}
                          fallback={
                            <div class="flex items-start gap-2">
                              <div class="min-w-0 flex-1">
                                <div class="flex items-center gap-2">
                                  <span class="truncate text-xs font-medium text-neutral-700 dark:text-neutral-200">
                                    {p.name}
                                  </span>
                                  {p.active && (
                                    <span class="shrink-0 rounded-full bg-emerald-100 px-1.5 py-0.5 text-[10px] font-semibold text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-400">
                                      Active
                                    </span>
                                  )}
                                </div>
                                <div class="truncate text-[11px] text-neutral-400 dark:text-neutral-500">
                                  {p.baseUrl}
                                </div>
                                <div class="text-[11px] text-neutral-400 dark:text-neutral-500">
                                  {p.hasKey ? "Key saved" : "No key"}
                                  {p.defaultModel ? ` · ${p.defaultModel}` : ""}
                                </div>
                              </div>
                              <div class="flex shrink-0 flex-col gap-1 text-right">
                                {!p.active && (
                                  <button
                                    onClick={() => void props.doSetActive(p.id)}
                                    class="text-[11px] font-normal text-neutral-500 transition hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                                  >
                                    Set active
                                  </button>
                                )}
                                <button
                                  onClick={() => props.startEdit(p)}
                                  class="text-[11px] font-normal text-neutral-500 transition hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                                >
                                  Edit
                                </button>
                                <button
                                  onClick={() => void props.doRemoveProvider(p.id)}
                                  class="text-[11px] font-normal text-red-600 transition hover:underline disabled:opacity-50 dark:text-red-400"
                                  disabled={p.active}
                                  title={p.active ? "Remove the active provider first" : undefined}
                                >
                                  Remove
                                </button>
                              </div>
                            </div>
                          }
                        >
                          <div class="space-y-2">
                            <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                              Name
                              <input
                                class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                                value={props.editName}
                                onInput={(e) => props.setEditName(e.currentTarget.value)}
                              />
                            </label>
                            <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                              Base URL
                              <input
                                class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                                value={props.editUrl}
                                onInput={(e) => props.setEditUrl(e.currentTarget.value)}
                              />
                            </label>
                            <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                              API key
                              <input
                                type="password"
                                class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                                placeholder={
                                  p.hasKey
                                    ? "saved — blank keeps it, type to replace"
                                    : "sk-… / fw_…"
                                }
                                value={props.editKey}
                                onInput={(e) => props.setEditKey(e.currentTarget.value)}
                              />
                              <span class="mt-1 block text-[10px] font-normal text-neutral-400 dark:text-neutral-500">
                                Stored in the OS keychain.
                              </span>
                            </label>
                            <Show when={p.hasKey}>
                              <button
                                onClick={() => void props.doClearProviderKey(p.id)}
                                class="text-[11px] font-normal text-red-600 transition hover:underline dark:text-red-400"
                              >
                                Clear saved key
                              </button>
                            </Show>
                            <div class="flex gap-2">
                              <button
                                onClick={() => void props.doUpdateProvider()}
                                class="rounded-md bg-neutral-900 px-3 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                              >
                                Save
                              </button>
                              <button
                                onClick={props.cancelEdit}
                                class="rounded-md border border-neutral-300 px-3 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                              >
                                Cancel
                              </button>
                            </div>
                          </div>
                        </Show>
                      </div>
                    )}
                  </For>
                </div>

                <Show when={props.addOpen}>
                  <div class="mt-2 space-y-2 rounded-lg border border-neutral-200 p-2 dark:border-neutral-700">
                    <Show when={props.modelsDevProviders.length > 0}>
                      <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        Pick from catalog (optional)
                        <select
                          class="mt-1 w-full rounded-lg border border-neutral-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200"
                          value={props.addPick}
                          onChange={(e) => {
                            const pick = e.currentTarget.value;
                            props.setAddPick(pick);
                            const chosen = props.modelsDevProviders.find((p) => p.id === pick);
                            if (chosen) {
                              // The pick is the source of truth: always overwrite the
                              // name/URL so switching catalog entries updates them.
                              // (Use "— custom —" to type a manual endpoint instead.)
                              props.setAddName(chosen.name);
                              props.setAddUrl(chosen.baseUrl);
                            }
                          }}
                        >
                          <option value="">— custom —</option>
                          <For each={props.modelsDevProviders}>
                            {(p) => <option value={p.id}>{p.name}</option>}
                          </For>
                        </select>
                      </label>
                    </Show>
                    <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                      Name
                      <input
                        class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                        placeholder="My provider"
                        value={props.addName}
                        onInput={(e) => props.setAddName(e.currentTarget.value)}
                      />
                    </label>
                    <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                      Base URL
                      <input
                        class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                        placeholder="https://api.example.com/inference/v1"
                        value={props.addUrl}
                        onInput={(e) => props.setAddUrl(e.currentTarget.value)}
                      />
                    </label>
                    <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                      API key (optional)
                      <input
                        type="password"
                        class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                        placeholder="sk-… / fw_…"
                        value={props.addKey}
                        onInput={(e) => props.setAddKey(e.currentTarget.value)}
                      />
                    </label>
                    <div class="flex gap-2">
                      <button
                        onClick={() => void props.doAddProvider()}
                        class="rounded-md bg-neutral-900 px-3 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                      >
                        Add
                      </button>
                      <button
                        onClick={() => {
                          props.setAddOpen(false);
                          props.setAddPick("");
                        }}
                        class="rounded-md border border-neutral-300 px-3 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                </Show>

                <Show when={props.providerError}>
                  <p class="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-400">
                    {props.providerError}
                  </p>
                </Show>
                <p class="mt-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Each provider has its own base URL, API key, and model list. The active one
                  is used for new chats.
                </p>
              </div>

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <div class="text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Model pricing &amp; context overrides
              </div>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Custom endpoints often don't report a context limit or prices in /models. Set
                them per model here; an override beats the endpoint's own values and the
                built-in defaults. Leave a field blank to fall back.
              </span>
              <select
                class="mt-2 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 disabled:opacity-60 dark:border-neutral-700 dark:bg-neutral-800"
                value={props.providers.length === 0 ? "" : props.overrideModel}
                onChange={(e) => props.setOverrideModel(e.currentTarget.value)}
                disabled={props.providers.length === 0}
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
              <Show when={props.overrideModel}>
                <div class="mt-2 grid grid-cols-2 gap-2">
                  <label class="col-span-2 block text-[11px] text-neutral-600 dark:text-neutral-300">
                    Context window (tokens)
                    <input
                      type="number"
                      min="1"
                      step="1"
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder={
                        props.overrideEffective?.contextWindow
                          ? String(props.overrideEffective!.contextWindow)
                          : "e.g. 262144"
                      }
                      value={props.overrideFor(props.overrideModel).contextWindow ?? ""}
                      onChange={(e) =>
                        props.setOverrideField(
                          props.overrideModel,
                          "contextWindow",
                          props.numFromInput(e.currentTarget.value),
                        )
                      }
                    />
                  </label>
                  <label class="block text-[11px] text-neutral-600 dark:text-neutral-300">
                    Input $/1M
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder={String(props.overrideEffective?.inputPerMillion ?? "")}
                      value={props.overrideFor(props.overrideModel).inputPerMillion ?? ""}
                      onChange={(e) =>
                        props.setOverrideField(
                          props.overrideModel,
                          "inputPerMillion",
                          props.numFromInput(e.currentTarget.value),
                        )
                      }
                    />
                  </label>
                  <label class="block text-[11px] text-neutral-600 dark:text-neutral-300">
                    Output $/1M
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder={String(props.overrideEffective?.outputPerMillion ?? "")}
                      value={props.overrideFor(props.overrideModel).outputPerMillion ?? ""}
                      onChange={(e) =>
                        props.setOverrideField(
                          props.overrideModel,
                          "outputPerMillion",
                          props.numFromInput(e.currentTarget.value),
                        )
                      }
                    />
                  </label>
                  <label class="block text-[11px] text-neutral-600 dark:text-neutral-300">
                    Cache read $/1M
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder={String(props.overrideEffective?.cacheReadPerMillion ?? "")}
                      value={props.overrideFor(props.overrideModel).cacheReadPerMillion ?? ""}
                      onChange={(e) =>
                        props.setOverrideField(
                          props.overrideModel,
                          "cacheReadPerMillion",
                          props.numFromInput(e.currentTarget.value),
                        )
                      }
                    />
                  </label>
                  <label class="block text-[11px] text-neutral-600 dark:text-neutral-300">
                    Cache write $/1M
                    <input
                      type="number"
                      min="0"
                      step="0.01"
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder={String(props.overrideEffective?.cacheWritePerMillion ?? "")}
                      value={props.overrideFor(props.overrideModel).cacheWritePerMillion ?? ""}
                      onChange={(e) =>
                        props.setOverrideField(
                          props.overrideModel,
                          "cacheWritePerMillion",
                          props.numFromInput(e.currentTarget.value),
                        )
                      }
                    />
                  </label>
                </div>
                <div class="mt-2 flex items-center justify-between gap-2">
                  <span class="text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                    <Show when={props.overrideEffective} fallback="Currently: unknown">
                      {(p) => (
                        <>
                          Currently: {formatTokens(p().contextWindow)} tok · {p().source}
                        </>
                      )}
                    </Show>
                  </span>
                  <Show when={props.modelOverrides[props.overrideModel]}>
                    <button
                      type="button"
                      onClick={() => props.clearOverride(props.overrideModel)}
                      class="text-[11px] text-red-600 transition hover:underline dark:text-red-400"
                    >
                      Clear override
                    </button>
                  </Show>
                </div>
              </Show>
            </div>

            <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
              Brave Search API key
              <input
                type="password"
                class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                placeholder={
                  props.hasBraveKeySaved
                    ? "••••••  saved in keychain — type to replace"
                    : "BSA… (optional — enables web search tools)"
                }
                value={props.braveKeyDraft}
                onInput={(e) => props.setBraveKeyDraft(e.currentTarget.value)}
              />
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Enables Brave's native search tools (web / news / images / videos / local /
                summarizer) — handled directly, no external server, so it works on Android too.
                Stored in the OS keychain.
              </span>
              <Show when={props.hasBraveKeySaved}>
                <button
                  onClick={async () => {
                    try {
                      await props.setBraveKey(null);
                      props.setHasBraveKeySaved(false);
                      props.setBraveKeyDraft("");
                    } catch (e) {
                      props.setSettingsError(String(e));
                    }
                  }}
                  class="mt-1 text-[11px] font-normal text-red-600 transition hover:underline dark:text-red-400"
                >
                  Remove saved key
                </button>
              </Show>
            </label>

            <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
              Explicit user preferences
              <textarea
                class="mt-1 min-h-20 w-full resize-y rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                placeholder="e.g. Prefer concise answers. Call me Sam. Always show units."
                value={props.preferences}
                onInput={(e) => props.setPreferences(e.currentTarget.value)}
              />
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Injected at the top of every system prompt and treated as authoritative.
                The assistant's inferred long-term memory lives in the Memory tab.
              </span>
            </label>

            <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
              <span class="flex items-center gap-2">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                  checked={props.echoReasoning}
                  onChange={(e) => props.setEchoReasoning(e.currentTarget.checked)}
                />
                Send reasoning back to the model between tool calls
              </span>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Required by DeepSeek thinking models; turn off if your provider rejects
                unknown fields.
              </span>
            </label>

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <label class="flex items-center gap-2 text-xs font-medium text-neutral-600 dark:text-neutral-300">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                  checked={props.reflectionEnabled}
                  onChange={(e) => props.setReflectionEnabled(e.currentTarget.checked)}
                />
                Consolidate memory after conversations go idle
              </label>
              <div class="mt-2 flex items-center gap-2">
                <span class="text-xs text-neutral-500 dark:text-neutral-400">Idle for</span>
                <input
                  type="number"
                  min="1"
                  class="w-20 rounded-lg border border-neutral-300 px-2 py-1 text-sm outline-none focus:border-neutral-500 disabled:opacity-50 dark:border-neutral-700 dark:bg-neutral-800"
                  disabled={!props.reflectionEnabled}
                  value={props.reflectionIdleMinutes}
                  onInput={(e) =>
                    props.setReflectionIdleMinutes(
                      Math.max(1, Number(e.currentTarget.value) || 1),
                    )
                  }
                />
                <span class="text-xs text-neutral-500 dark:text-neutral-400">minutes</span>
              </div>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                A background pass reads the conversation and rewrites the Markdown memory
                files. Spends tokens; cost is tracked separately.
              </span>
            </div>

            <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
              Shell working directory
              <input
                class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                placeholder="(default) /storage/emulated/0/PiChat"
                value={props.shellWorkspaceDir}
                onInput={(e) => props.setShellWorkspaceDir(e.currentTarget.value)}
              />
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Where the bash / file tools work on your files. On Android this is a directory
                under shared storage that the agent drives through Termux; on desktop the shell
                inherits the app's working directory unless you set one here. Each tool call
                runs in a fresh shell (no persistent <code>cd</code> / <code>export</code>).
              </span>
            </label>

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <label class="flex items-center gap-2 text-xs font-medium text-neutral-600 dark:text-neutral-300">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                  checked={props.syncEnabled}
                  disabled={props.syncBusy}
                  onChange={(e) => void props.toggleSync(e.currentTarget.checked)}
                />
                Cloud backup &amp; sync
              </label>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Back up and keep your chats, memory, and settings in sync across devices via
                Supabase. Off by default — the app stays fully local until you sign in. Your
                data is scoped to your account.
              </span>

              <Show when={props.syncEnabled}>
                <Show
                  when={props.syncStatusData?.loggedIn}
                  fallback={
                    <div class="mt-3 space-y-2">
                      <div class="flex rounded-lg border border-neutral-300 p-0.5 dark:border-neutral-700">
                        {(["signin", "signup"] as const).map((m) => (
                          <button
                            type="button"
                            onClick={() => {
                              props.setSyncAuthMode(m);
                              props.setSyncError(null);
                              props.setSyncNotice(null);
                            }}
                            class={`flex-1 rounded-md px-2 py-1 text-[11px] font-medium transition ${
                              props.syncAuthMode === m
                                ? "bg-neutral-900 text-white dark:bg-white dark:text-neutral-900"
                                : "text-neutral-500 hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                            }`}
                          >
                            {m === "signin" ? "Sign in" : "Create account"}
                          </button>
                        ))}
                      </div>
                      <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        Email
                        <input
                          type="email"
                          class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                          placeholder="you@example.com"
                          value={props.syncEmail}
                          onInput={(e) => props.setSyncEmail(e.currentTarget.value)}
                        />
                      </label>
                      <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        Password
                        <input
                          type="password"
                          class="mt-1 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                          placeholder="••••••••"
                          value={props.syncPassword}
                          onInput={(e) => props.setSyncPassword(e.currentTarget.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") {
                              void (props.syncAuthMode === "signin"
                                ? props.submitSyncSignIn()
                                : props.submitSyncSignUp());
                            }
                          }}
                        />
                      </label>
                      <button
                        onClick={() =>
                          void (props.syncAuthMode === "signin"
                            ? props.submitSyncSignIn()
                            : props.submitSyncSignUp())
                        }
                        disabled={props.syncBusy}
                        class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                      >
                        {props.syncAuthMode === "signin" ? "Sign in" : "Create account"}
                      </button>
                      <Show when={props.syncNotice}>
                        <p class="mt-2 rounded-lg bg-emerald-50 px-3 py-2 text-xs text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-400">
                          {props.syncNotice}
                        </p>
                      </Show>
                    </div>
                  }
                >
                  <div class="mt-3 flex items-center gap-2">
                    <span class="truncate text-xs text-neutral-600 dark:text-neutral-300">
                      Signed in as <span class="font-medium">{props.syncStatusData?.email}</span>
                    </span>
                    <button
                      onClick={() => void props.doSyncSignOut()}
                      disabled={props.syncBusy}
                      class="ml-auto shrink-0 text-[11px] font-normal text-red-600 transition hover:underline disabled:opacity-50 dark:text-red-400"
                    >
                      Sign out
                    </button>
                  </div>
                  <button
                    onClick={() => void props.doSyncNow()}
                    disabled={props.syncBusy || props.syncStatusData?.syncing}
                    class="mt-2 rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                  >
                    Sync now
                  </button>

                  <div class="mt-4 border-t border-neutral-200 pt-3 dark:border-neutral-700">
                    <div class="flex items-center gap-2">
                      <span class="text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        End-to-end encryption
                      </span>
                      <span
                        class={`rounded-full px-2 py-0.5 text-[10px] font-semibold ${
                          props.syncStatusData?.encryption
                            ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-400"
                            : props.syncStatusData?.locked
                              ? "bg-amber-100 text-amber-700 dark:bg-amber-950/50 dark:text-amber-400"
                              : "bg-neutral-200 text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400"
                        }`}
                      >
                        {props.syncStatusData?.encryption
                          ? "On"
                          : props.syncStatusData?.locked
                            ? "Locked"
                            : "Off"}
                      </span>
                    </div>
                    <p class="mt-1 text-[11px] leading-snug text-neutral-400 dark:text-neutral-500">
                      {props.syncStatusData?.encryption
                        ? "Data sent to Supabase is ciphertext — only this device can read it."
                        : props.syncStatusData?.locked
                          ? "This backup is encrypted. Paste its recovery code to unlock it on this device."
                          : "Optional: encrypt the sync mirror so Supabase only ever sees ciphertext."}
                    </p>
                    <Show
                      when={props.syncStatusData?.encryption}
                      fallback={
                        <Show
                          when={props.syncStatusData?.locked}
                          fallback={
                            <div class="mt-2">
                              <button
                                onClick={() => void props.enableEncryption()}
                                disabled={props.syncEncBusy}
                                class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                              >
                                {props.syncEncBusy ? "Encrypting…" : "Enable encryption"}
                              </button>
                              <p class="mt-1 text-[10px] leading-snug text-neutral-400 dark:text-neutral-500">
                                A key is generated and kept in your OS keychain. You'll get a
                                recovery code to use on other devices.
                              </p>
                            </div>
                          }
                        >
                          <div class="mt-2 space-y-2">
                            <p class="text-[10px] leading-snug text-neutral-400 dark:text-neutral-500">
                              This backup is encrypted by another device. Paste its recovery
                              code to read it here.
                            </p>
                            <input
                              type="password"
                              placeholder="Recovery code"
                              value={props.syncRecoveryInput}
                              onInput={(e) => props.setSyncRecoveryInput(e.currentTarget.value)}
                              class="w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            />
                            <button
                              onClick={() => void props.unlockEncryption()}
                              disabled={props.syncEncBusy}
                              class="rounded-md bg-neutral-900 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                            >
                              {props.syncEncBusy ? "Unlocking…" : "Unlock"}
                            </button>
                          </div>
                        </Show>
                      }
                    >
                      <div class="mt-2 space-y-2">
                        <Show
                          when={props.syncRecoveryShown}
                          fallback={
                            <button
                              onClick={() => void props.showRecoveryCode()}
                              class="text-[11px] font-normal text-neutral-500 underline-offset-2 transition hover:text-neutral-800 disabled:opacity-50 dark:text-neutral-400 dark:hover:text-neutral-200"
                            >
                              Show recovery code
                            </button>
                          }
                        >
                          <p class="text-[10px] leading-snug text-neutral-400 dark:text-neutral-500">
                            Save this to unlock the backup on another device:
                          </p>
                          <code class="block select-all break-all rounded-lg bg-neutral-100 px-2 py-1.5 font-mono text-[10px] text-neutral-700 dark:bg-neutral-800 dark:text-neutral-300">
                            {props.syncRecoveryShown}
                          </code>
                        </Show>
                        <button
                          onClick={() => void props.disableEncryption()}
                          disabled={props.syncEncBusy}
                          class="block text-[11px] font-normal text-neutral-500 underline-offset-2 transition hover:text-red-600 disabled:opacity-50 dark:text-neutral-400 dark:hover:text-red-400"
                        >
                          Disable
                        </button>
                      </div>
                    </Show>
                  </div>
                </Show>

                <Show when={props.syncStatusData?.syncing || props.syncStatusData?.phase}>
                  <div class="mt-2">
                    <div class="flex items-baseline justify-between gap-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                      <span>{props.syncLabel(props.syncStatusData)}</span>
                      <span class="tabular-nums">
                        ↑{props.syncStatusData?.pushed ?? 0} · ↓{props.syncStatusData?.pulled ?? 0}{" "}
                        · {props.syncStatusData?.pending ?? 0} pending
                      </span>
                    </div>
                    <div class="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-neutral-200 dark:bg-neutral-700">
                      <div
                        class={`h-full rounded-full bg-neutral-500 transition-all duration-500 dark:bg-neutral-400 ${
                          props.syncStatusData?.phase === "pull" ? "animate-pulse" : ""
                        }`}
                        style={{ width: `${props.syncBarWidth(props.syncStatusData)}%` }}
                      />
                    </div>
                  </div>
                </Show>

                <p class="mt-2 text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Last sync: {props.formatLastSync(props.syncStatusData?.lastSyncAt ?? null)} ·{" "}
                  {props.syncStatusData?.pending ?? 0} pending
                </p>

                <Show when={props.syncStatusData?.lastError}>
                  <p class="mt-1 text-[11px] font-normal text-red-600 dark:text-red-400">
                    {props.syncStatusData?.lastError}
                  </p>
                </Show>

                <Show when={props.syncError}>
                  <p class="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-400">
                    {props.syncError}
                  </p>
                </Show>
              </Show>
            </div>

            <Show when={props.settingsError}>
              <p class="rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-400">
                {props.settingsError}
              </p>
            </Show>
          </div>
          <p class="mt-5 text-right text-[11px] text-neutral-400 dark:text-neutral-500">
            Changes are saved when the dialog closes.
          </p>
          <div class="mt-2 flex justify-end">
            <Dialog.CloseButton class="rounded-md bg-neutral-900 px-4 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300">
              Done
            </Dialog.CloseButton>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog>
  );
}
