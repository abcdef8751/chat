import { createEffect, createSignal, For, Index, onCleanup, Show, type JSX } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import type {
  AndroidStorageStatus,
  McpServerConfig,
  McpServerStatus,
  McpSnapshot,
  ModelInfo,
  ModelOverride,
  ModelsDevProvider,
  Pricing,
  ProviderInfo,
  SkillInfo,
  SyncStatus,
} from "../lib/api";
import {
  androidRequestStorageAccess,
  androidStorageStatus,
  listSkills,
  mcpListServers,
  mcpReload,
  mcpRemoveServer,
  mcpSaveServer,
  reloadSkills,
  skillsDir,
} from "../lib/api";
import { BackArrowIcon } from "../lib/icons";
import { parseModalities, resolveInputModalities } from "../lib/capabilities";
import { isAndroidDevice } from "../lib/platform";
import { formatTokens } from "../lib/turns";

/// Friendly badge text for a models.dev modality token.
function modalityLabel(modality: string): string {
  switch (modality) {
    case "image":
      return "images";
    case "pdf":
    case "document":
      return "documents";
    default:
      return modality;
  }
}

/// Modality tokens offered in the capabilities editor (models.dev vocabulary).
const INPUT_MODALITY_OPTIONS = ["text", "image", "audio", "video"];
const OUTPUT_MODALITY_OPTIONS = ["text", "image", "audio"];

/// Small capability chip used in the model configurator (read-only when rendered
/// standalone, clickable when wrapped in a button by the editor).
function CapabilityBadge(props: { tone: "on" | "off" | "none" | "muted"; children: JSX.Element }) {
  return (
    <span
      class={`rounded-full px-1.5 py-0.5 text-[10px] font-medium ${
        props.tone === "on"
          ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-400"
          : props.tone === "off"
            ? "bg-neutral-200 text-neutral-500 dark:bg-neutral-700 dark:text-neutral-400"
            : props.tone === "none"
              ? "bg-neutral-100 text-neutral-400 dark:bg-neutral-800 dark:text-neutral-500"
              : "bg-neutral-100 text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400"
      }`}
    >
      {props.children}
    </span>
  );
}

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
  /**
   * Model used to auto-name a chat from its first message. Empty = the chat's
   * own model. Optional (with `setTitleModel`) so the dialog compiles before
   * the app shell plumbs it.
   */
  titleModel?: string;
  setTitleModel?: (value: string) => void;
  overrideModel: string;
  setOverrideModel: (value: string) => void;
  overrideFor: (id: string) => ModelOverride;
  setOverrideField: (
    id: string,
    field: keyof ModelOverride,
    value: ModelOverride[keyof ModelOverride],
  ) => void;
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
  /**
   * Global kill-switch for the cron scheduler. Default true; off stops every
   * job from firing (jobs keep their config, they just don't run).
   */
  cronEnabled: boolean;
  setCronEnabled: (value: boolean) => void;
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
  /// The model the pricing/context override block is currently pointed at, or
  /// `null` when no provider/model is selected (or known).
  const selectedModel = () =>
    props.modelOptions().find((m) => m.id === props.overrideModel) ?? null;

  // --- Android "all files access" (shared storage) ---
  // The file tools work under shared storage on Android, which needs
  // MANAGE_EXTERNAL_STORAGE. It is not a runtime prompt: the OS grants it from a
  // system Settings screen, so the status is re-checked whenever the dialog
  // opens (and the user can re-check via the button after returning).
  const [storage, setStorage] = createSignal<AndroidStorageStatus | null>(null);
  const [storageBusy, setStorageBusy] = createSignal(false);
  const [storageError, setStorageError] = createSignal<string | null>(null);

  const refreshStorage = async () => {
    // Desktop (and any build without the command) must simply not show the row.
    if (!isAndroidDevice()) {
      setStorage(null);
      return;
    }
    try {
      const status = await androidStorageStatus();
      // The dialog may have closed while the query was in flight: drop the
      // result instead of caching it, or the next open renders it for a frame
      // before the fresh check lands (the exact stale-flash we clear on close).
      if (!props.settingsOpen) return;
      setStorage(status);
      setStorageError(null);
    } catch {
      // Command unavailable / platform said no: hide the row rather than show a
      // broken state inside Settings.
      if (props.settingsOpen) setStorage(null);
    }
  };

  createEffect(() => {
    if (!props.settingsOpen) {
      // Drop the cached status on close: reopening would otherwise render the
      // previous session's "Granted" for a frame (before the fresh check lands),
      // which is exactly the state the user may have just revoked.
      setStorage(null);
      setStorageError(null);
      return;
    }
    setStorageError(null);
    void refreshStorage();
  });

  const requestStorage = async () => {
    setStorageBusy(true);
    setStorageError(null);
    try {
      await androidRequestStorageAccess();
      // The grant happens in the system UI; this only confirms the screen
      // opened. Re-check on return via the button / window focus below.
    } catch (e) {
      setStorageError(String(e));
    } finally {
      setStorageBusy(false);
    }
  };

  // Coming back from the system screen re-focuses the webview on Android.
  if (typeof window !== "undefined") {
    const onFocus = () => {
      if (props.settingsOpen && storage()) void refreshStorage();
    };
    window.addEventListener("focus", onFocus);
    onCleanup(() => window.removeEventListener("focus", onFocus));
  }

  // --- MCP servers ---------------------------------------------------------
  // Config + live connection status in one snapshot. Loading never blocks the
  // dialog: the card renders immediately and fills in asynchronously.
  function blankMcpServer(): McpServerConfig {
    return {
      id: "",
      name: "",
      enabled: true,
      transport: "http",
      url: "",
      command: "",
      args: [],
      env: {},
      headers: {},
    };
  }

  /** One editable key→value row (headers or env, depending on the transport). */
  type McpKeyValueRow = { key: string; value: string };

  const rowsFromMap = (map: Record<string, string> | undefined): McpKeyValueRow[] =>
    Object.entries(map ?? {}).map(([key, value]) => ({ key, value }));

  /** Collapse rows to a map: blank keys are dropped, later duplicates win. */
  const rowsToMap = (rows: McpKeyValueRow[]): Record<string, string> => {
    const out: Record<string, string> = {};
    for (const row of rows) {
      const key = row.key.trim();
      if (!key) continue;
      out[key] = row.value;
    }
    return out;
  };

  const [mcpServers, setMcpServers] = createSignal<McpServerConfig[]>([]);
  const [mcpStatus, setMcpStatus] = createSignal<McpServerStatus[]>([]);
  const [mcpTools, setMcpTools] = createSignal(0);
  const [mcpLoading, setMcpLoading] = createSignal(false);
  const [mcpLoaded, setMcpLoaded] = createSignal(false);
  const [mcpBusy, setMcpBusy] = createSignal(false);
  const [mcpError, setMcpError] = createSignal<string | null>(null);
  const [mcpFormOpen, setMcpFormOpen] = createSignal(false);
  const [mcpForm, setMcpForm] = createSignal<McpServerConfig>(blankMcpServer());
  // Raw text for the space-separated stdio args. Keeping it as text (not a
  // derived `args.join(" ")`) stops the input from eating a trailing space
  // while the user is still typing the next argument.
  const [mcpArgsText, setMcpArgsText] = createSignal("");
  // Editable key→value rows for the map the current transport uses (headers for
  // `http`, env for `stdio`). Rows are kept separately from the form signal so
  // a blank row can exist while the user is typing its key.
  const [mcpRows, setMcpRows] = createSignal<McpKeyValueRow[]>([]);

  const applyMcpSnapshot = (snap: McpSnapshot) => {
    setMcpServers(snap.servers ?? []);
    setMcpStatus(snap.status?.servers ?? []);
    setMcpTools(snap.status?.tools ?? 0);
  };

  const mcpStatusFor = (id: string) => mcpStatus().find((s) => s.id === id) ?? null;

  const mcpStatusText = (server: McpServerConfig): string => {
    const st = mcpStatusFor(server.id);
    if (st?.connected)
      return `connected · ${st.tools} ${st.tools === 1 ? "tool" : "tools"}`;
    if (st?.error) return st.error;
    return server.enabled ? "not connected" : "disabled";
  };

  const mcpStatusIsError = (server: McpServerConfig): boolean => {
    const st = mcpStatusFor(server.id);
    return !!st && !st.connected && !!st.error;
  };

  const refreshMcp = async () => {
    setMcpLoading(true);
    try {
      const snap = await mcpListServers();
      if (!props.settingsOpen) return;
      applyMcpSnapshot(snap);
      setMcpError(null);
    } catch (e) {
      if (props.settingsOpen) setMcpError(String(e));
    } finally {
      setMcpLoading(false);
      setMcpLoaded(true);
    }
  };

  const reloadMcp = async () => {
    setMcpLoading(true);
    setMcpError(null);
    try {
      const snap = await mcpReload();
      if (!props.settingsOpen) return;
      applyMcpSnapshot(snap);
    } catch (e) {
      if (props.settingsOpen) setMcpError(String(e));
    } finally {
      setMcpLoading(false);
      setMcpLoaded(true);
    }
  };

  const openMcpForm = (server?: McpServerConfig) => {
    const next = server
      ? {
          ...server,
          args: [...server.args],
          env: { ...(server.env ?? {}) },
          headers: { ...(server.headers ?? {}) },
        }
      : blankMcpServer();
    setMcpForm(next);
    setMcpArgsText(next.args.join(" "));
    setMcpRows(rowsFromMap(next.transport === "stdio" ? next.env : next.headers));
    setMcpFormOpen(true);
    setMcpError(null);
  };

  const patchMcpForm = (patch: Partial<McpServerConfig>) =>
    setMcpForm((f) => ({ ...f, ...patch }));

  /** Stash the visible rows into the outgoing map before switching transports. */
  const changeMcpTransport = (transport: "http" | "stdio") => {
    const current = mcpForm().transport;
    if (current === transport) return;
    const saved = rowsToMap(mcpRows());
    const next =
      current === "stdio"
        ? { ...mcpForm(), transport, env: saved }
        : { ...mcpForm(), transport, headers: saved };
    setMcpForm(next);
    setMcpRows(rowsFromMap(transport === "stdio" ? next.env : next.headers));
  };

  const updateMcpRow = (index: number, patch: Partial<McpKeyValueRow>) =>
    setMcpRows((rows) =>
      rows.map((row, i) => (i === index ? { ...row, ...patch } : row)),
    );

  const addMcpRow = () => setMcpRows((rows) => [...rows, { key: "", value: "" }]);

  const removeMcpRow = (index: number) =>
    setMcpRows((rows) => rows.filter((_, i) => i !== index));

  const saveMcp = async () => {
    const form = mcpForm();
    const name = form.name.trim();
    if (!name) {
      setMcpError("Give the MCP server a name.");
      return;
    }
    setMcpBusy(true);
    setMcpError(null);
    const configured = rowsToMap(mcpRows());
    try {
      const snap = await mcpSaveServer({
        ...form,
        name,
        args:
          form.transport === "stdio"
            ? mcpArgsText().split(/\s+/).filter(Boolean)
            : form.args,
        // Only the visible transport's map comes from the rows; the other one is
        // whatever was stashed when the transport was last switched.
        headers: form.transport === "http" ? configured : form.headers,
        env: form.transport === "stdio" ? configured : form.env,
      });
      applyMcpSnapshot(snap);
      setMcpFormOpen(false);
      setMcpForm(blankMcpServer());
      setMcpArgsText("");
      setMcpRows([]);
    } catch (e) {
      setMcpError(String(e));
    } finally {
      setMcpBusy(false);
    }
  };

  const toggleMcpEnabled = async (server: McpServerConfig, enabled: boolean) => {
    setMcpBusy(true);
    setMcpError(null);
    try {
      applyMcpSnapshot(await mcpSaveServer({ ...server, enabled }));
    } catch (e) {
      setMcpError(String(e));
    } finally {
      setMcpBusy(false);
    }
  };

  const removeMcp = async (id: string) => {
    setMcpBusy(true);
    setMcpError(null);
    try {
      applyMcpSnapshot(await mcpRemoveServer(id));
      if (mcpForm().id === id) setMcpFormOpen(false);
    } catch (e) {
      setMcpError(String(e));
    } finally {
      setMcpBusy(false);
    }
  };

  // --- Skills (slash commands) --------------------------------------------
  const [skills, setSkills] = createSignal<SkillInfo[]>([]);
  const [skillsPath, setSkillsPath] = createSignal("");
  const [skillsLoading, setSkillsLoading] = createSignal(false);
  const [skillsError, setSkillsError] = createSignal<string | null>(null);
  const [skillsLoaded, setSkillsLoaded] = createSignal(false);

  const refreshSkills = async (bust: boolean) => {
    setSkillsLoading(true);
    setSkillsError(null);
    try {
      // Two independent calls: a backend without the directory command should
      // still list whatever skills it knows about.
      try {
        setSkills(bust ? await reloadSkills() : await listSkills());
      } catch (e) {
        setSkillsError(String(e));
      }
      try {
        const dir = await skillsDir();
        if (dir) setSkillsPath(dir);
      } catch {
        // No directory command (older backend): the folder line is optional.
      }
    } finally {
      setSkillsLoading(false);
      setSkillsLoaded(true);
    }
  };

  // Lazy: load both cards when the dialog opens, drop their transient error
  // state on close (the lists themselves refresh on the next open).
  createEffect(() => {
    if (!props.settingsOpen) {
      setMcpFormOpen(false);
      setMcpError(null);
      setMcpBusy(false);
      setSkillsError(null);
      return;
    }
    void refreshMcp();
    void refreshSkills(false);
  });

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
              <Show when={selectedModel()}>
                {(m) => {
                  const parsed = () => parseModalities(m().modalities);
                  const over = () => props.overrideFor(m().id);
                  // Effective capabilities: user override → models.dev → the
                  // text/text/reasoning defaults. Mirrors the backend's
                  // `resolve_capabilities`, so a custom provider with no catalog
                  // entry can still be declared here.
                  const inputModalities = (): string[] =>
                    resolveInputModalities(m().modalities, m().attachment, over().inputModalities);
                  const outputModalities = (): string[] =>
                    over().outputModalities ?? parsed()?.output ?? ["text"];
                  const reasoningSupported = (): boolean =>
                    over().reasoning ?? m().reasoning ?? true;
                  const isOverridden = () =>
                    over().inputModalities != null ||
                    over().outputModalities != null ||
                    over().reasoning != null;
                  const toggle = (cur: string[], mod: string): string[] =>
                    cur.includes(mod) ? cur.filter((x) => x !== mod) : [...cur, mod];
                  return (
                    <div class="mt-2 rounded-lg border border-neutral-200 bg-neutral-50 px-2.5 py-2 dark:border-neutral-700 dark:bg-neutral-800/50">
                      <div class="flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                        <span>Capabilities</span>
                        <Show when={isOverridden()}>
                          <span class="normal-case text-amber-600 dark:text-amber-400">
                            (customized)
                          </span>
                        </Show>
                      </div>
                      <p class="mt-0.5 text-[10px] font-normal text-neutral-400 dark:text-neutral-500">
                        Click to toggle. Custom providers report nothing, so these
                        default to text-in / text-out with reasoning supported.
                      </p>
                      <div class="mt-1.5 space-y-1">
                        <div class="flex items-start gap-2">
                          <span class="w-14 shrink-0 pt-px text-[10px] font-medium uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                            Input
                          </span>
                          <div class="flex flex-wrap gap-1">
                            <For each={INPUT_MODALITY_OPTIONS}>
                              {(mod) => (
                                <button
                                  type="button"
                                  class="cursor-pointer"
                                  title={`Toggle ${mod} input`}
                                  onClick={() =>
                                    props.setOverrideField(
                                      m().id,
                                      "inputModalities",
                                      toggle(inputModalities(), mod),
                                    )
                                  }
                                >
                                  <CapabilityBadge
                                    tone={inputModalities().includes(mod) ? "on" : "off"}
                                  >
                                    {modalityLabel(mod)}
                                  </CapabilityBadge>
                                </button>
                              )}
                            </For>
                          </div>
                        </div>
                        <div class="flex items-start gap-2">
                          <span class="w-14 shrink-0 pt-px text-[10px] font-medium uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                            Output
                          </span>
                          <div class="flex flex-wrap gap-1">
                            <For each={OUTPUT_MODALITY_OPTIONS}>
                              {(mod) => (
                                <button
                                  type="button"
                                  class="cursor-pointer"
                                  title={`Toggle ${mod} output`}
                                  onClick={() =>
                                    props.setOverrideField(
                                      m().id,
                                      "outputModalities",
                                      toggle(outputModalities(), mod),
                                    )
                                  }
                                >
                                  <CapabilityBadge
                                    tone={outputModalities().includes(mod) ? "on" : "off"}
                                  >
                                    {modalityLabel(mod)}
                                  </CapabilityBadge>
                                </button>
                              )}
                            </For>
                          </div>
                        </div>
                        <div class="flex items-center gap-2">
                          <span class="w-14 shrink-0 text-[10px] font-medium uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                            Reasoning
                          </span>
                          <button
                            type="button"
                            class="cursor-pointer"
                            title="Toggle reasoning support"
                            onClick={() =>
                              props.setOverrideField(m().id, "reasoning", !reasoningSupported())
                            }
                          >
                            <CapabilityBadge tone={reasoningSupported() ? "on" : "off"}>
                              {reasoningSupported() ? "supported" : "not supported"}
                            </CapabilityBadge>
                          </button>
                        </div>
                      </div>
                    </div>
                  );
                }}
              </Show>
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

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <label class="block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Chat titles
                <select
                  class="mt-1 w-full rounded-lg border border-neutral-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-neutral-500 disabled:opacity-60 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200"
                  value={props.titleModel ?? ""}
                  disabled={!props.setTitleModel}
                  onChange={(e) => props.setTitleModel?.(e.currentTarget.value)}
                >
                  <option value="">Chat's own model</option>
                  <For each={props.modelOptions()}>
                    {(m) => <option value={m.id}>{m.name?.trim() || m.id}</option>}
                  </For>
                </select>
                <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                  Titles are generated from the first message. Empty uses the chat's own
                  model.
                </span>
              </label>
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

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <div class="flex flex-wrap items-center gap-2">
                <span class="text-xs font-medium text-neutral-600 dark:text-neutral-300">
                  MCP servers
                </span>
                <div class="ml-auto flex items-center gap-1.5">
                  <button
                    onClick={() => void reloadMcp()}
                    disabled={mcpLoading()}
                    class="rounded-md border border-neutral-300 px-2 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 disabled:opacity-50 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                  >
                    {mcpLoading() ? "Loading…" : "Reload"}
                  </button>
                  <button
                    onClick={() => {
                      if (mcpFormOpen()) {
                        setMcpFormOpen(false);
                        setMcpError(null);
                      } else {
                        openMcpForm();
                      }
                    }}
                    class="rounded-md bg-neutral-900 px-2 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                  >
                    {mcpFormOpen() ? "Cancel" : "+ Add"}
                  </button>
                </div>
              </div>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Model Context Protocol servers add their tools to chats.{" "}
                {mcpTools() > 0
                  ? `${mcpTools()} ${mcpTools() === 1 ? "tool" : "tools"} available.`
                  : "No tools connected yet."}
              </span>

              <div class="mt-2 space-y-2">
                <Show when={mcpServers().length > 0}>
                  <For each={mcpServers()}>
                    {(s) => (
                      <div class="rounded-lg border border-neutral-200 p-2 dark:border-neutral-700">
                        <div class="flex flex-wrap items-center gap-2">
                          <input
                            type="checkbox"
                            title={s.enabled ? "Disable this server" : "Enable this server"}
                            class="h-3.5 w-3.5 shrink-0 rounded border-neutral-300 dark:border-neutral-700"
                            checked={s.enabled}
                            disabled={mcpBusy()}
                            onChange={(e) =>
                              void toggleMcpEnabled(s, e.currentTarget.checked)
                            }
                          />
                          <span class="min-w-0 flex-1 truncate text-xs font-medium text-neutral-700 dark:text-neutral-200">
                            {s.name}
                          </span>
                          <span class="shrink-0 rounded-full bg-neutral-100 px-1.5 py-0.5 text-[10px] font-medium text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400">
                            {s.transport}
                          </span>
                          <button
                            onClick={() => openMcpForm(s)}
                            class="shrink-0 text-[11px] text-neutral-500 transition hover:text-neutral-800 dark:text-neutral-400 dark:hover:text-neutral-200"
                          >
                            Edit
                          </button>
                          <button
                            onClick={() => void removeMcp(s.id)}
                            disabled={mcpBusy()}
                            class="shrink-0 text-[11px] text-red-600 transition hover:underline disabled:opacity-50 dark:text-red-400"
                          >
                            Remove
                          </button>
                        </div>
                        <p
                          class={`mt-1 whitespace-pre-wrap break-words text-[10px] leading-snug ${
                            mcpStatusIsError(s)
                              ? "text-red-600 dark:text-red-400"
                              : "text-neutral-400 dark:text-neutral-500"
                          }`}
                        >
                          {mcpStatusText(s)}
                        </p>
                      </div>
                    )}
                  </For>
                </Show>
                <Show when={mcpLoaded() && mcpServers().length === 0}>
                  <p class="text-[11px] text-neutral-400 dark:text-neutral-500">
                    No MCP servers configured.
                  </p>
                </Show>
              </div>

              <Show when={mcpFormOpen()}>
                <div class="mt-2 space-y-2 rounded-lg border border-neutral-200 bg-neutral-50 p-2 dark:border-neutral-700 dark:bg-neutral-800/40">
                  <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                    Name
                    <input
                      class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                      placeholder="My MCP server"
                      value={mcpForm().name}
                      onInput={(e) => patchMcpForm({ name: e.currentTarget.value })}
                    />
                  </label>
                  <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                    Transport
                    <select
                      class="mt-1 w-full rounded-lg border border-neutral-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200"
                      value={mcpForm().transport}
                      onChange={(e) =>
                        changeMcpTransport(e.currentTarget.value as "http" | "stdio")
                      }
                    >
                      <option value="http">http</option>
                      <option value="stdio">stdio</option>
                    </select>
                  </label>
                  <Show
                    when={mcpForm().transport === "http"}
                    fallback={
                      <>
                        <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                          Command
                          <input
                            class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            placeholder="npx"
                            value={mcpForm().command}
                            onInput={(e) =>
                              patchMcpForm({ command: e.currentTarget.value })
                            }
                          />
                        </label>
                        <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                          Arguments (space-separated)
                          <input
                            class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            placeholder="-y @modelcontextprotocol/server-filesystem /path"
                            value={mcpArgsText()}
                            onInput={(e) => setMcpArgsText(e.currentTarget.value)}
                          />
                        </label>
                      </>
                    }
                  >
                    <label class="block text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                      URL
                      <input
                        class="mt-1 w-full rounded-lg border border-neutral-300 px-2 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                        placeholder="https://example.com/mcp"
                        value={mcpForm().url}
                        onInput={(e) => patchMcpForm({ url: e.currentTarget.value })}
                      />
                    </label>
                  </Show>

                  <div class="space-y-1">
                    <div class="flex items-center gap-2">
                      <span class="text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                        {mcpForm().transport === "http"
                          ? "Headers"
                          : "Environment variables"}
                      </span>
                      <button
                        onClick={addMcpRow}
                        class="ml-auto rounded-md border border-neutral-300 px-2 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      >
                        + Add row
                      </button>
                    </div>
                    <Show when={mcpRows().length === 0}>
                      <p class="text-[10px] text-neutral-400 dark:text-neutral-500">
                        {mcpForm().transport === "http"
                          ? "Sent with every request, e.g. Authorization → Bearer …"
                          : "Passed to the process, e.g. API_KEY → …"}
                      </p>
                    </Show>
                    <Index each={mcpRows()}>
                      {(row, i) => (
                        <div class="flex items-center gap-1">
                          <input
                            class="min-w-0 flex-1 rounded-lg border border-neutral-300 px-2 py-1 text-xs outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            placeholder={
                              mcpForm().transport === "http" ? "Header" : "Name"
                            }
                            value={row().key}
                            onInput={(e) =>
                              updateMcpRow(i, { key: e.currentTarget.value })
                            }
                          />
                          <input
                            class="min-w-0 flex-1 rounded-lg border border-neutral-300 px-2 py-1 text-xs outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                            placeholder="Value"
                            value={row().value}
                            onInput={(e) =>
                              updateMcpRow(i, { value: e.currentTarget.value })
                            }
                          />
                          <button
                            onClick={() => removeMcpRow(i)}
                            title="Remove row"
                            class="shrink-0 px-1 text-sm leading-none text-neutral-400 transition hover:text-neutral-700 dark:text-neutral-500 dark:hover:text-neutral-300"
                          >
                            ×
                          </button>
                        </div>
                      )}
                    </Index>
                  </div>

                  <label class="flex items-center gap-2 text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
                    <input
                      type="checkbox"
                      class="h-3.5 w-3.5 rounded border-neutral-300 dark:border-neutral-700"
                      checked={mcpForm().enabled}
                      onChange={(e) =>
                        patchMcpForm({ enabled: e.currentTarget.checked })
                      }
                    />
                    Enabled
                  </label>
                  <div class="flex items-center gap-2">
                    <button
                      onClick={() => void saveMcp()}
                      disabled={mcpBusy() || !mcpForm().name.trim()}
                      class="rounded-md bg-neutral-900 px-3 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                    >
                      {mcpBusy() ? "Saving…" : "Save"}
                    </button>
                    <button
                      onClick={() => {
                        setMcpFormOpen(false);
                        setMcpError(null);
                      }}
                      class="rounded-md border border-neutral-300 px-3 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                    >
                      Cancel
                    </button>
                  </div>
                </div>
              </Show>
              <Show when={mcpError()}>
                <p class="mt-1.5 whitespace-pre-wrap break-words text-[11px] text-red-600 dark:text-red-400">
                  {mcpError()}
                </p>
              </Show>
            </div>

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <div class="flex flex-wrap items-center gap-2">
                <span class="text-xs font-medium text-neutral-600 dark:text-neutral-300">
                  Skills
                </span>
                <button
                  onClick={() => void refreshSkills(true)}
                  disabled={skillsLoading()}
                  class="ml-auto rounded-md border border-neutral-300 px-2 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 disabled:opacity-50 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                >
                  {skillsLoading() ? "Loading…" : "Reload"}
                </button>
              </div>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Create a Markdown file in the folder below. In the composer, start a
                message with <code>/</code> to invoke a skill.
              </span>
              <Show when={skillsPath()}>
                <code class="mt-2 block select-all break-all rounded-lg bg-neutral-100 px-2 py-1.5 font-mono text-[10px] text-neutral-700 dark:bg-neutral-800 dark:text-neutral-300">
                  {skillsPath()}
                </code>
              </Show>
              <ul class="mt-2 space-y-1.5">
                <For each={skills()}>
                  {(s) => (
                    <li class="rounded-md border border-neutral-200 px-2 py-1.5 dark:border-neutral-700">
                      <div class="flex items-baseline gap-2">
                        <span class="min-w-0 truncate text-xs font-medium text-neutral-700 dark:text-neutral-200">
                          {s.name}
                        </span>
                        <code class="shrink-0 text-[10px] text-neutral-400 dark:text-neutral-500">
                          /{s.id}
                        </code>
                      </div>
                      <Show when={s.description}>
                        <p class="mt-0.5 text-[11px] leading-snug text-neutral-500 dark:text-neutral-400">
                          {s.description}
                        </p>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
              <Show when={skillsLoaded() && !skillsLoading() && skills().length === 0}>
                <p class="mt-1 text-[11px] text-neutral-400 dark:text-neutral-500">
                  No skills yet.
                </p>
              </Show>
              <Show when={skillsError()}>
                <p class="mt-1.5 whitespace-pre-wrap break-words text-[11px] text-red-600 dark:text-red-400">
                  {skillsError()}
                </p>
              </Show>
            </div>

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

            <div class="rounded-lg border border-amber-300 bg-amber-50 p-3 dark:border-amber-900/60 dark:bg-amber-950/30">
              <label class="flex items-center gap-2 text-xs font-medium text-neutral-600 dark:text-neutral-300">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                  checked={props.autoApproveTools}
                  onChange={(e) => props.setAutoApproveTools(e.currentTarget.checked)}
                />
                Auto-approve tool calls (YOLO)
              </label>
              <span class="mt-1 block text-[11px] font-normal text-amber-700 dark:text-amber-400">
                Gated tools — <code>bash</code>, <code>read_file</code>, <code>write_file</code> —
                run <strong>without</strong> per-call approval. Only enable this when you trust the
                model and the workspace it can touch.
              </span>
            </div>

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

            <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <label class="flex items-center gap-2 text-xs font-medium text-neutral-600 dark:text-neutral-300">
                <input
                  type="checkbox"
                  class="h-4 w-4 rounded border-neutral-300 dark:border-neutral-700"
                  checked={props.cronEnabled}
                  onChange={(e) => props.setCronEnabled(e.currentTarget.checked)}
                />
                Scheduled jobs
              </label>
              <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                Run scheduled prompts in the background (local-only; runs spend tokens).
                Turn this off to pause every job without deleting them.
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

            {/* Android only: shared-storage ("all files") access for the file
                tools. Hidden entirely when the command is unavailable (desktop,
                older builds) so Settings stays clean. */}
            <Show when={storage()}>
              {(st) => (
                <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
                  <div class="flex items-center justify-between gap-2">
                    <span class="text-xs font-medium text-neutral-600 dark:text-neutral-300">
                      Storage access
                    </span>
                    <span
                      class={`shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-semibold ${
                        st().granted
                          ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-400"
                          : "bg-amber-100 text-amber-700 dark:bg-amber-950/50 dark:text-amber-400"
                      }`}
                    >
                      {st().granted ? "Granted" : st().canRequest ? "Not granted" : "Unavailable"}
                    </span>
                  </div>
                  <span class="mt-1 block text-[11px] font-normal text-neutral-400 dark:text-neutral-500">
                    {st().granted
                      ? "The file tools can read and write shared storage (Termux runs under the same grant)."
                      : st().canRequest
                        ? "Without it the file tools can only reach the app's own directories. Android grants this from a system settings screen, not a popup."
                        : "This Android version has no all-files access setting; the file tools stay inside the app's own directories."}
                  </span>
                  <div class="mt-2 flex items-center gap-2">
                    <Show when={!st().granted && st().canRequest}>
                      <button
                        onClick={() => void requestStorage()}
                        disabled={storageBusy()}
                        class="rounded-md bg-neutral-900 px-3 py-1 text-[11px] font-medium text-white transition hover:bg-neutral-700 disabled:opacity-50 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                      >
                        {storageBusy() ? "Opening…" : "Open settings"}
                      </button>
                    </Show>
                    <button
                      onClick={() => void refreshStorage()}
                      class="rounded-md border border-neutral-300 px-3 py-1 text-[11px] font-medium text-neutral-600 transition hover:bg-neutral-100 dark:border-neutral-700 dark:text-neutral-300 dark:hover:bg-neutral-800"
                    >
                      Re-check
                    </button>
                  </div>
                  <Show when={storageError()}>
                    <p class="mt-1.5 text-[11px] text-red-600 dark:text-red-400">
                      {storageError()}
                    </p>
                  </Show>
                </div>
              )}
            </Show>

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
                        ↑{props.syncStatusData?.pushed ?? 0} ·{" "}
                        {props.syncStatusData?.phase === "pull" &&
                        (props.syncStatusData?.pullTotal ?? 0) > 0
                          ? `↓${props.syncStatusData?.pulled ?? 0}/${props.syncStatusData?.pullTotal ?? 0}`
                          : `↓${props.syncStatusData?.pulled ?? 0}`}{" "}
                        · {props.syncStatusData?.pending ?? 0} pending
                      </span>
                    </div>
                    <div class="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-neutral-200 dark:bg-neutral-700">
                      <div
                        class={`h-full rounded-full bg-neutral-500 transition-all duration-500 dark:bg-neutral-400 ${
                          props.syncStatusData?.phase === "pull" &&
                          !(props.syncStatusData?.pullTotal ?? 0)
                            ? "animate-pulse"
                            : ""
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
