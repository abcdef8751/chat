import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import type {
  CreateCronPayload,
  CronJob,
  CronPreview,
  CronRun,
  UpdateCronPayload,
} from "../lib/api";
import { cronPreview } from "../lib/api";
import { BackArrowIcon, ClockIcon } from "../lib/icons";
import { formatCost } from "../lib/turns";

/// The Scheduled jobs dialog: the job list (with run-now / edit / delete and
/// the enabled/notify toggles), an inline new/edit form with live schedule
/// preview, and the selected job's recent runs. Mirrors MemoryPage's dialog
/// pattern: full-screen on narrow viewports, a centred bounded card on wide.
///
/// Form state (drafts, the open/editing flag, delete confirmation) is local —
/// only what App needs for routing/events is lifted (the open flag, the
/// selected job id, the job list, the runs list, and the busy/error signals).
export function CronPage(props: {
  cronOpen: boolean;
  setCronOpen: (open: boolean) => void;
  wideViewport: boolean;
  fullscreenInsetStyle: () => Record<string, string>;
  cronJobs: CronJob[];
  cronRuns: CronRun[];
  loadCronRuns: (jobId: string | null) => void;
  cronSelectedId: string | null;
  setCronSelectedId: (id: string | null) => void;
  cronBusy: boolean;
  cronError: string | null;
  setCronError: (value: string | null) => void;
  onCreateCron: (payload: CreateCronPayload) => Promise<void>;
  onUpdateCron: (id: string, patch: Omit<UpdateCronPayload, "id">) => Promise<void>;
  onDeleteCron: (id: string) => Promise<void>;
  onRunCronNow: (id: string) => Promise<void>;
  /// The open conversation — every new job is pinned to it (the backend
  /// rejects a null conversation, so Save is disabled without one).
  activeId: string | null;
}) {
  // --- Local form state ---------------------------------------------------
  // The schedule is composed from a preset (Hourly / Daily / Weekly) or taken
  // raw from the "Custom" input. Daily/Weekly carry their time (and weekday)
  // as separate drafts so the values survive preset switching.
  type Preset = "hourly" | "daily" | "weekly" | "custom";
  const [formOpen, setFormOpen] = createSignal(false);
  const [editingJobId, setEditingJobId] = createSignal<string | null>(null);
  const [preset, setPreset] = createSignal<Preset>("daily");
  const [dailyTime, setDailyTime] = createSignal("09:00");
  const [weeklyDay, setWeeklyDay] = createSignal("Mon");
  const [weeklyTime, setWeeklyTime] = createSignal("09:00");
  const [customSchedule, setCustomSchedule] = createSignal("0 9 * * *");
  const [formTitle, setFormTitle] = createSignal("");
  const [formPrompt, setFormPrompt] = createSignal("");
  const [formToolPolicy, setFormToolPolicy] = createSignal<"readonly" | "autoapprove">(
    "readonly",
  );
  const [formNotify, setFormNotify] = createSignal(true);
  const [confirmDeleteId, setConfirmDeleteId] = createSignal<string | null>(null);
  // Live validation + next-run preview (`cron_preview`, debounced below).
  const [preview, setPreview] = createSignal<CronPreview | null>(null);

  // The schedule string the form currently describes, in the backend's
  // preset-or-5-field vocabulary.
  const effectiveSchedule = createMemo(() => {
    switch (preset()) {
      case "hourly":
        return "hourly";
      case "daily":
        return `daily@${dailyTime()}`;
      case "weekly":
        // The backend's preset vocabulary uses lowercase weekday tokens
        // (`weekly@mon:09:00`); the select shows capitalized labels.
        return `weekly@${weeklyDay().toLowerCase()}:${weeklyTime()}`;
      case "custom":
        return customSchedule().trim();
    }
  });

  // Debounced (250ms) preview: hit `cron_preview` as the user types, but never
  // more than once per pause, and drop responses for superseded inputs (a
  // stale preview for an earlier schedule must not overwrite the current one).
  let previewSeq = 0;
  createEffect(() => {
    const sched = effectiveSchedule();
    if (!formOpen()) return;
    const seq = ++previewSeq;
    const t = setTimeout(() => {
      void cronPreview(sched)
        .then((p) => {
          if (seq === previewSeq) setPreview(p);
        })
        .catch((e) => {
          // Invoke error (e.g. unparseable custom cron) surfaces as a preview
          // error so the Save button disables just like a validation error.
          if (seq === previewSeq)
            setPreview({ schedule: sched, description: "", nextRunAt: null, error: String(e) });
        });
    }, 250);
    onCleanup(() => {
      clearTimeout(t);
      // Invalidate any in-flight response when the input changed (or the form
      // closed) before it resolved.
      if (seq === previewSeq) previewSeq++;
    });
  });

  // Closing the dialog closes the form too, so a reopen always starts at the
  // job list (with a fresh selection). Also reset the stale preview.
  createEffect(() => {
    if (!props.cronOpen) closeForm();
  });

  // Pick a default selection when the dialog opens: the first job, or heal a
  // selection whose job was deleted out from under it.
  createEffect(() => {
    if (!props.cronOpen) return;
    const sel = props.cronSelectedId;
    if (props.cronJobs.length > 0 && (!sel || !props.cronJobs.some((j) => j.id === sel))) {
      props.setCronSelectedId(props.cronJobs[0].id);
    }
  });

  // Load the selected job's recent runs whenever the selection changes while
  // the dialog is open. `loadCronRuns` lives in App (so the cron-updated event
  // handler can refresh it too) and guards against out-of-order responses.
  createEffect(() => {
    if (!props.cronOpen) return;
    props.loadCronRuns(props.cronSelectedId);
  });

  const selectedJob = () => props.cronJobs.find((j) => j.id === props.cronSelectedId) ?? null;

  // --- Form open / pre-fill -------------------------------------------------

  function closeForm() {
    setFormOpen(false);
    setEditingJobId(null);
    setPreview(null);
  }

  function startCreate() {
    props.setCronError(null);
    setConfirmDeleteId(null);
    setPreset("daily");
    setDailyTime("09:00");
    setWeeklyDay("Mon");
    setWeeklyTime("09:00");
    setCustomSchedule("0 9 * * *");
    setFormTitle("");
    setFormPrompt("");
    setFormToolPolicy("readonly");
    setFormNotify(true);
    setEditingJobId(null);
    setPreview(null);
    setFormOpen(true);
  }

  /// Pre-fill the form from an existing job. A stored schedule is the
  /// **normalized** expression (the backend canonicalizes presets on write), so
  /// the shape is parsed back into a preset when it matches `hourly`, a daily
  /// `M H * * *`, or a weekly `M H * * DOW`; anything else lands in Custom.
  function startEdit(job: CronJob) {
    props.setCronError(null);
    setConfirmDeleteId(null);
    const daily = job.schedule.match(/^(\d{1,2}) (\d{1,2}) \* \* \*$/);
    const weekly = job.schedule.match(/^(\d{1,2}) (\d{1,2}) \* \* ([A-Za-z0-9]{3}|[0-7])$/);
    const pad = (n: string) => n.padStart(2, "0");
    const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    if (job.schedule === "0 * * * *") {
      setPreset("hourly");
    } else if (daily) {
      setPreset("daily");
      setDailyTime(`${pad(daily[2])}:${pad(daily[1])}`);
    } else if (weekly) {
      setPreset("weekly");
      // The select's options are "Mon"…"Sun"; map names or numeric DOW (0/7 =
      // Sunday, matching cron).
      const token = weekly[3];
      const byName = WEEKDAYS.find((d) => d.toLowerCase().startsWith(token.toLowerCase()));
      const numeric = Number(token);
      setWeeklyDay(
        byName ?? (Number.isInteger(numeric) ? (WEEKDAYS[numeric % 7] ?? "Mon") : "Mon"),
      );
      setWeeklyTime(`${pad(weekly[2])}:${pad(weekly[1])}`);
    } else {
      setPreset("custom");
      setCustomSchedule(job.schedule);
    }
    setFormTitle(job.title);
    setFormPrompt(job.prompt);
    setFormToolPolicy(job.toolPolicy);
    setFormNotify(job.notify);
    setEditingJobId(job.id);
    setPreview(null); // let the debounced preview refill
    setFormOpen(true);
  }

  // Save is disabled until the schedule preview has resolved VALID (an error,
  // or no preview yet, keeps it disabled), the prompt is non-empty, and —
  // for a new job — a conversation is open to own it.
  const canSave = () => {
    if (props.cronBusy) return false;
    if (!formPrompt().trim()) return false;
    const p = preview();
    if (!p || !!p.error) return false;
    if (!editingJobId() && !props.activeId) return false;
    return true;
  };

  async function saveForm() {
    const prompt = formPrompt().trim();
    const p = preview();
    if (!canSave() || !p) return;
    const base = {
      title: formTitle().trim() || null,
      schedule: effectiveSchedule(),
      prompt,
      toolPolicy: formToolPolicy(),
      notify: formNotify(),
    };
    const jobId = editingJobId();
    if (jobId) {
      await props.onUpdateCron(jobId, base);
    } else {
      // The backend requires the owning conversation: always send the active
      // chat's id (a null is rejected).
      await props.onCreateCron({
        conversationId: props.activeId ?? null,
        ...base,
      });
    }
    closeForm();
  }

  async function runNow(jobId: string) {
    await props.onRunCronNow(jobId);
  }

  async function deleteJob(jobId: string) {
    setConfirmDeleteId(null);
    await props.onDeleteCron(jobId);
  }

  // --- Small pure helpers (schedule/time rendering) ------------------------

  /// Human-readable description of a **stored** (normalized) schedule. The
  /// backend canonicalizes presets on write, so this recognizes `hourly`'s
  /// normalized form, `M H * * *` (daily) and `M H * * DOW` (weekly, numeric or
  /// 3-letter name). Anything more complex returns null, and the caller falls
  /// back to the raw expression. (Mirrors `cron::schedule::describe`.)
  function describeSchedule(sched: string): string | null {
    if (sched === "hourly" || sched === "0 * * * *") return "Every hour";
    const fields = sched.trim().split(/\s+/);
    if (fields.length !== 5) return null;
    const [min, hour, dom, mon, dow] = fields;
    if (dom !== "*" || mon !== "*") return null;
    if (min === "*" && hour === "*" && dow === "*") return "Every minute";
    const minute = Number(min);
    const h = Number(hour);
    if (!Number.isInteger(minute) || !Number.isInteger(h)) return null;
    if (minute < 0 || minute > 59 || h < 0 || h > 23) return null;
    const at = `${String(h).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;
    const WEEKDAYS: Record<string, string> = {
      "0": "Sunday", "1": "Monday", "2": "Tuesday", "3": "Wednesday",
      "4": "Thursday", "5": "Friday", "6": "Saturday", "7": "Sunday",
      sun: "Sunday", mon: "Monday", tue: "Tuesday", wed: "Wednesday",
      thu: "Thursday", fri: "Friday", sat: "Saturday",
    };
    if (dow === "*") return `Daily at ${at}`;
    const day = WEEKDAYS[dow.toLowerCase()];
    return day ? `Weekly on ${day} at ${at}` : null;
  }

  /// Compact absolute time (e.g. "Oct 18, 09:00"); `null` renders "never".
  function formatWhen(ms: number | null): string {
    if (!ms) return "never";
    return new Date(ms).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /// Next run: relative when it's within a day (the useful case), absolute
  /// otherwise.
  function formatNextRun(ms: number | null): string {
    if (!ms) return "never";
    const diff = ms - Date.now();
    if (diff > 0 && diff < 24 * 3600 * 1000) {
      const mins = Math.round(diff / 60000);
      if (mins < 60) return `in ${mins} min`;
      return `in ${Math.round(mins / 60)} h`;
    }
    return formatWhen(ms);
  }

  const badgeClass =
    "rounded bg-neutral-100 px-1.5 py-0.5 text-[10px] text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400";
  const inputClass =
    "rounded-md border border-neutral-300 bg-white px-2 py-1.5 text-xs text-neutral-700 outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200";
  const smallButtonClass =
    "rounded-md border border-neutral-300 px-2.5 py-1.5 text-[11px] text-neutral-600 transition hover:bg-neutral-100 disabled:cursor-not-allowed disabled:opacity-40 dark:border-neutral-600 dark:text-neutral-300 dark:hover:bg-neutral-800";

  return (
    <Dialog open={props.cronOpen} onOpenChange={props.setCronOpen}>
      <Dialog.Portal>
        <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
        <Dialog.Content
          class={
            // Same shape as MemoryPage: full-screen on narrow viewports (with
            // system-bar insets), a bounded centred card on wide ones. The
            // dialog itself scrolls, so the job list / form / runs can exceed
            // the card on a short window.
            props.wideViewport
              ? "fixed left-1/2 top-1/2 z-50 flex h-[70vh] w-full max-w-3xl -translate-x-1/2 -translate-y-1/2 flex-col overflow-y-auto rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900"
              : "fixed inset-0 z-50 flex h-full w-full flex-col overflow-y-auto bg-white p-5 focus:outline-none dark:bg-neutral-900"
          }
          style={props.fullscreenInsetStyle()}
        >
          <div class="flex items-center gap-2">
            {/* Mobile (full-screen) exit arrow — hidden on wide viewports, where
                the centred card keeps its Close button. */}
            <Dialog.CloseButton
              title="Close scheduled jobs"
              aria-label="Close scheduled jobs"
              class="shrink-0 rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-100 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
            >
              <BackArrowIcon />
            </Dialog.CloseButton>
            <Dialog.Title class="text-base font-semibold">Scheduled jobs</Dialog.Title>
          </div>
          <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
            Jobs run a prompt in a conversation on a schedule, in the background, without
            opening the app. Runs post a message to that chat and can send an OS
            notification. Each run spends tokens like a normal turn.
          </Dialog.Description>

          <Show when={props.cronError}>
            <p class="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-400">
              {props.cronError}
            </p>
          </Show>

          {/* --- New / edit form (inline card) --- */}
          <Show when={formOpen()}>
            <div class="mt-4 rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
              <p class="text-xs font-medium">
                {editingJobId() ? "Edit job" : "New job"}
              </p>

              <label class="mt-3 block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Title <span class="font-normal text-neutral-400 dark:text-neutral-500">(optional — derived from the prompt)</span>
                <input
                  class={`${inputClass} mt-1 w-full`}
                  placeholder="Daily morning briefing"
                  value={formTitle()}
                  onInput={(e) => setFormTitle(e.currentTarget.value)}
                />
              </label>

              <div class="mt-3">
                <p class="text-xs font-medium text-neutral-600 dark:text-neutral-300">Schedule</p>
                <div class="mt-1 flex flex-wrap items-center gap-2">
                  <select
                    class={inputClass}
                    value={preset()}
                    onChange={(e) => setPreset(e.currentTarget.value as Preset)}
                  >
                    <option value="hourly">Hourly</option>
                    <option value="daily">Daily</option>
                    <option value="weekly">Weekly</option>
                    <option value="custom">Custom</option>
                  </select>
                  <Show when={preset() === "daily"}>
                    <input
                      type="time"
                      class={inputClass}
                      value={dailyTime()}
                      onInput={(e) => setDailyTime(e.currentTarget.value)}
                    />
                  </Show>
                  <Show when={preset() === "weekly"}>
                    <select
                      class={inputClass}
                      value={weeklyDay()}
                      onChange={(e) => setWeeklyDay(e.currentTarget.value)}
                    >
                      <For each={["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]}>
                        {(d) => <option value={d}>{d}</option>}
                      </For>
                    </select>
                    <input
                      type="time"
                      class={inputClass}
                      value={weeklyTime()}
                      onInput={(e) => setWeeklyTime(e.currentTarget.value)}
                    />
                  </Show>
                  <Show when={preset() === "custom"}>
                    <input
                      type="text"
                      class={`${inputClass} w-44 font-mono`}
                      placeholder="0 9 * * *"
                      value={customSchedule()}
                      title="5-field cron expression: minute hour day-of-month month day-of-week"
                      onInput={(e) => setCustomSchedule(e.currentTarget.value)}
                    />
                  </Show>
                  <code class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-[10px] text-neutral-500 dark:bg-neutral-800 dark:text-neutral-400">
                    {effectiveSchedule() || "…"}
                  </code>
                </div>
                {/* Live validation + next-run preview from the backend. An
                    error disables Save; while the preview hasn't resolved yet
                    Save stays disabled too. */}
                <Show when={preview()?.error}>
                  <p class="mt-1.5 whitespace-pre-wrap break-words text-[11px] text-red-600 dark:text-red-400">
                    {preview()!.error}
                  </p>
                </Show>
                <Show when={!preview()?.error && preview()}>
                  <p class="mt-1.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                    {preview()!.description}
                    <Show when={preview()!.nextRunAt != null}>
                      {' '}· next run {formatNextRun(preview()!.nextRunAt)}
                    </Show>
                  </p>
                </Show>
              </div>

              <label class="mt-3 block text-xs font-medium text-neutral-600 dark:text-neutral-300">
                Prompt
                <textarea
                  class="mt-1 min-h-20 w-full resize-y rounded-lg border border-neutral-300 bg-white px-3 py-2 text-xs outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-100"
                  placeholder="Summarize the last day of conversation and…"
                  value={formPrompt()}
                  onInput={(e) => setFormPrompt(e.currentTarget.value)}
                />
              </label>

              <div class="mt-3 rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
                <p class="text-xs font-medium text-neutral-600 dark:text-neutral-300">Host tools</p>
                <label class="mt-2 flex items-start gap-2 text-[11px] text-neutral-600 dark:text-neutral-300">
                  <input
                    type="radio"
                    name="cron-tool-policy"
                    class="mt-0.5 h-3.5 w-3.5"
                    checked={formToolPolicy() === "readonly"}
                    onChange={() => setFormToolPolicy("readonly")}
                  />
                  <span>
                    Block host tools (bash/read/write)
                    <span class="block text-neutral-400 dark:text-neutral-500">
                      The job's model can still use web search, memory, and fetch.
                    </span>
                  </span>
                </label>
                <label class="mt-2 flex items-start gap-2 text-[11px] text-neutral-600 dark:text-neutral-300">
                  <input
                    type="radio"
                    name="cron-tool-policy"
                    class="mt-0.5 h-3.5 w-3.5"
                    checked={formToolPolicy() === "autoapprove"}
                    onChange={() => setFormToolPolicy("autoapprove")}
                  />
                  <span>
                    Auto-approve host tools
                    <span class="block text-neutral-400 dark:text-neutral-500">
                      bash / read_file / write_file run without approval. Only for jobs you
                      trust with your files.
                    </span>
                  </span>
                </label>
              </div>

              <label class="mt-3 flex items-center gap-2 text-[11px] text-neutral-600 dark:text-neutral-300">
                <input
                  type="checkbox"
                  class="h-3.5 w-3.5 rounded border-neutral-300 dark:border-neutral-700"
                  checked={formNotify()}
                  onChange={(e) => setFormNotify(e.currentTarget.checked)}
                />
                Send an OS notification when this job runs
              </label>

              <Show when={!editingJobId() && !props.activeId}>
                <p class="mt-2 text-[11px] text-neutral-400 dark:text-neutral-500">
                  Open a conversation first — a new job needs a chat to post its runs into.
                </p>
              </Show>

              <div class="mt-3 flex items-center justify-end gap-2">
                <button
                  onClick={closeForm}
                  class={`${smallButtonClass} border-transparent text-neutral-500 hover:bg-neutral-100 dark:text-neutral-400`}
                >
                  Cancel
                </button>
                <button
                  onClick={() => void saveForm()}
                  disabled={!canSave()}
                  title={
                    props.cronBusy
                      ? "Working…"
                      : preview()?.error
                        ? "Fix the schedule first"
                        : !formPrompt().trim()
                          ? "Enter a prompt"
                          : ""
                  }
                  class="rounded-md bg-neutral-900 px-4 py-1.5 text-xs font-medium text-white transition hover:bg-neutral-700 disabled:cursor-not-allowed disabled:opacity-40 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                >
                  {editingJobId() ? (props.cronBusy ? "Saving…" : "Save changes") : props.cronBusy ? "Creating…" : "Create job"}
                </button>
              </div>
            </div>
          </Show>

          {/* --- Job list --- */}
          <div class="mt-4">
            <div class="flex items-center justify-between gap-3">
              <p class="text-xs font-medium">Jobs</p>
              <Show when={!formOpen()}>
                <button onClick={startCreate} class={smallButtonClass}>
                  + New job
                </button>
              </Show>
            </div>

            <Show
              when={props.cronJobs.length > 0}
              fallback={
                <p class="mt-2 rounded-lg border border-dashed border-neutral-300 p-4 text-center text-xs text-neutral-400 dark:border-neutral-700 dark:text-neutral-500">
                  No scheduled jobs yet. Create one above.
                </p>
              }
            >
              <div class="mt-2 space-y-2">
                <For each={props.cronJobs}>
                  {(job) => {
                    const desc = describeSchedule(job.schedule);
                    const selected = props.cronSelectedId === job.id;
                    return (
                      <div
                        class={`rounded-lg border p-3 transition ${
                          selected
                            ? "border-neutral-400 dark:border-neutral-600"
                            : "border-neutral-200 dark:border-neutral-700"
                        }`}
                      >
                        <div class="flex items-start justify-between gap-3">
                          {/* Clicking the info selects the job (shows its runs). */}
                          <button
                            onClick={() =>
                              props.setCronSelectedId(selected ? null : job.id)
                            }
                            class="min-w-0 flex-1 text-left"
                            title={selected ? "Hide runs" : "Show recent runs"}
                          >
                            <span class="flex items-center gap-1.5">
                              <span class="truncate text-xs font-medium">{job.title}</span>
                              <Show when={!job.enabled}>
                                <span class={badgeClass}>paused</span>
                              </Show>
                              <span class={badgeClass}>
                                {job.toolPolicy === "readonly"
                                  ? "no host tools"
                                  : "auto-approve tools"}
                              </span>
                            </span>
                            <span class="mt-1 flex items-center gap-1.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                              {desc ?? <code class="font-mono">{job.schedule}</code>}
                              <Show when={desc !== null}>
                                <code class="rounded bg-neutral-100 px-1 py-0.5 font-mono text-[10px] text-neutral-400 dark:bg-neutral-800 dark:text-neutral-500">
                                  {job.schedule}
                                </code>
                              </Show>
                            </span>
                            <span class="mt-0.5 block text-[11px] text-neutral-400 dark:text-neutral-500">
                              next {formatNextRun(job.nextRunAt)} · last {formatWhen(job.lastRunAt)} · {job.runCount} {job.runCount === 1 ? "run" : "runs"}
                            </span>
                          </button>
                          <div class="flex shrink-0 items-center gap-1">
                            <button
                              onClick={() => void runNow(job.id)}
                              disabled={props.cronBusy}
                              title="Run this job once, immediately, in the background"
                              class={smallButtonClass}
                            >
                              Run now
                            </button>
                            <button
                              onClick={() => startEdit(job)}
                              disabled={props.cronBusy}
                              class={smallButtonClass}
                            >
                              Edit
                            </button>
                            <Show
                              when={confirmDeleteId() !== job.id}
                              fallback={
                                <div class="flex items-center gap-1 rounded-md border border-red-300 px-1.5 py-1 dark:border-red-900/60">
                                  <span class="text-[10px] text-red-600 dark:text-red-400">
                                    Delete {job.title}?
                                  </span>
                                  <button
                                    onClick={() => void deleteJob(job.id)}
                                    disabled={props.cronBusy}
                                    class="rounded bg-red-600 px-1.5 py-0.5 text-[10px] text-white transition hover:bg-red-500"
                                  >
                                    Delete
                                  </button>
                                  <button
                                    onClick={() => setConfirmDeleteId(null)}
                                    class="text-[10px] text-neutral-500 transition hover:underline dark:text-neutral-400"
                                  >
                                    Cancel
                                  </button>
                                </div>
                              }
                            >
                              <button
                                onClick={() => setConfirmDeleteId(job.id)}
                                disabled={props.cronBusy}
                                title="Delete this job (past runs are kept on the transcript)"
                                class={`${smallButtonClass} text-red-600 hover:border-red-300 dark:text-red-400`}
                              >
                                Delete
                              </button>
                            </Show>
                          </div>
                        </div>
                        <div class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-neutral-100 pt-2 dark:border-neutral-800">
                          <label class="flex items-center gap-1.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                            <input
                              type="checkbox"
                              class="h-3.5 w-3.5 rounded border-neutral-300 dark:border-neutral-700"
                              checked={job.enabled}
                              title={job.enabled ? "Pause this job" : "Resume this job"}
                              onChange={(e) =>
                                void props.onUpdateCron(job.id, {
                                  enabled: e.currentTarget.checked,
                                })
                              }
                            />
                            enabled
                          </label>
                          <label class="flex items-center gap-1.5 text-[11px] text-neutral-500 dark:text-neutral-400">
                            <input
                              type="checkbox"
                              class="h-3.5 w-3.5 rounded border-neutral-300 dark:border-neutral-700"
                              checked={job.notify}
                              title="Send an OS notification when this job runs"
                              onChange={(e) =>
                                void props.onUpdateCron(job.id, {
                                  notify: e.currentTarget.checked,
                                })
                              }
                            />
                            notify
                          </label>
                        </div>
                      </div>
                    );
                  }}
                </For>
              </div>
            </Show>
          </div>

          {/* --- Recent runs for the selected job --- */}
          <div class="mt-4">
            <Show
              when={selectedJob()}
              fallback={
                <p class="text-[11px] text-neutral-400 dark:text-neutral-500">
                  Select a job to see its recent runs.
                </p>
              }
            >
              <div class="rounded-lg border border-neutral-200 p-3 dark:border-neutral-700">
                <div class="flex items-center gap-2">
                  <ClockIcon class="h-3.5 w-3.5 text-neutral-400 dark:text-neutral-500" />
                  <p class="truncate text-xs font-medium">
                    Recent runs — {selectedJob()!.title}
                  </p>
                </div>
                <Show
                  when={props.cronRuns.length > 0}
                  fallback={
                    <p class="mt-2 text-[11px] text-neutral-400 dark:text-neutral-500">
                      No runs yet. Run it now or wait for the next scheduled fire.
                    </p>
                  }
                >
                  <ul class="mt-2 space-y-1.5">
                    <For each={props.cronRuns}>
                      {(run) => (
                        <li class="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 text-[11px]">
                          <span class="text-neutral-500 dark:text-neutral-400">
                            {formatWhen(run.startedAt)}
                          </span>
                          <span class="text-neutral-400 dark:text-neutral-500">
                            {run.stopReason ?? "in progress"}
                          </span>
                          <span class="text-neutral-400 dark:text-neutral-500">
                            {run.cost != null ? formatCost(run.cost) : "—"}
                          </span>
                          <Show when={run.error}>
                            <span class="w-full break-words text-red-600 dark:text-red-400">
                              {run.error}
                            </span>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </div>
            </Show>
          </div>

          <div class="mt-4 flex items-center justify-end">
            <Dialog.CloseButton class="rounded-md border border-neutral-300 px-4 py-1.5 text-sm text-neutral-700 transition hover:bg-neutral-100 dark:border-neutral-600 dark:text-neutral-200 dark:hover:bg-neutral-800">
              Close
            </Dialog.CloseButton>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog>
  );
}