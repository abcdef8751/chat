import { createSignal, onMount, Show } from "solid-js";
import { systemInfo, type SystemInfo } from "../lib/api";

/// The running build + system-prompt revision as a tiny header chip.
///
/// The same id is rendered into the system prompt, so the model knows which
/// build and instruction revision it is answering under (the existing
/// instruction-diff note tells it when that changes). This is purely a
/// diagnostic affordance: it renders nothing while the command is in flight
/// and nothing at all if it fails (an older backend without `system_info`, or
/// a broken IPC bridge) — a missing command must never break the header.
export function SystemBadge() {
  const [info, setInfo] = createSignal<SystemInfo | null>(null);

  onMount(() => {
    systemInfo()
      .then((value) => setInfo(value))
      .catch(() => setInfo(null));
  });

  return (
    <Show when={info()}>
      {(system) => (
        <span
          class="hidden items-center gap-1 rounded-md px-1.5 py-1 text-[11px] text-neutral-500 lg:flex dark:text-neutral-400"
          title={`${system().id} — identifies the running app build and system-prompt revision; it changes when either is updated.`}
          aria-label={`System id ${system().id}`}
          data-system-id={system().id}
        >
          <span>sys</span>
          <span class="font-mono">{system().promptHash}</span>
        </span>
      )}
    </Show>
  );
}
