import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { Conversation } from "../lib/api";
import { CloseIcon, ClockIcon, MarkAvatar } from "../lib/icons";

/// The chat-list drawer: header + new-chat dialog, search, list items with
/// inline rename/delete, plus the narrow-viewport edge-swipe gesture.
export function Sidebar(props: {
  sidebarOpen: boolean;
  setSidebarOpen: (open: boolean) => void;
  wideViewport: boolean;
  insets: { top: number; left: number; right: number; bottom: number };
  onClosed: () => void;
  visibleConversations: Conversation[];
  activeId: string | null;
  setActiveId: (id: string) => void;
  /// Start an empty chat immediately. Naming is automatic now: the title is
  /// generated from the first message, so there is no dialog to fill in.
  createNewChat: () => void;
  searchQuery: string;
  setSearchQuery: (value: string) => void;
  renameId: string | null;
  renameValue: string;
  setRenameId: (id: string | null) => void;
  setRenameValue: (value: string) => void;
  commitRename: (id: string) => void;
  confirmDeleteId: string | null;
  setConfirmDeleteId: (id: string | null) => void;
  removeConversation: (id: string) => void;
  /// Conversations owning at least one enabled job — the tiny clock glyph next
  /// to their title (informational only, never interactive).
  cronConversationIds: ReadonlySet<string>;
  /// Conversations that produced output while the user was looking elsewhere.
  /// Each gets a small amber dot next to its title.
  unseenConversationIds: ReadonlySet<string>;
  /// Pin/unpin a chat (it moves into/out of the Pinned section).
  onTogglePin: (c: Conversation) => void;
}) {
  const drawerHidden = () => !props.wideViewport && !props.sidebarOpen;

  // NOTE: the drawer deliberately does NOT move focus into the search field on
  // open. Focusing a text input raises the Android soft keyboard, which would
  // cover the chat list the moment the drawer appears. The search field stays
  // reachable by tapping it, which is when the IME should come up. Desktop is
  // unaffected: the sidebar is always in flow at `lg`, so no focus is stolen.

  // Swipe from the left edge to open the off-canvas drawer (narrow viewports
  // only). Pointer-based: a drag starting within ~32px of the left edge that
  // becomes horizontally dominant and crosses ~56px toggles the drawer — a
  // right swipe opens it, a left swipe closes it. Vertical drags (scrolling
  // the message / conversation lists) are left untouched: the gesture only
  // engages once horizontal movement clearly wins, and it never calls
  // preventDefault, so native scrolling still flows.
  createEffect(() => {
    if (typeof window === "undefined") return;
    if (props.wideViewport) return;
    // Touch-only: the drawer has a hamburger/backdrop for mouse users, and
    // treating drags as touch avoids hijacking text selection with a mouse.
    const EDGE = 64; // px from the left edge where an opening drag may begin
    const THRESHOLD = 56; // px of horizontal travel needed to trigger

    let tracking = false;
    let horizontal = false;
    let startX = 0;
    let startY = 0;
    let startId = -1;

    const onTouchStart = (e: TouchEvent) => {
      if (props.wideViewport) return;
      const t = e.touches[0];
      if (!t) return;
      const fromLeftEdge = t.clientX <= EDGE;
      // Only begin from the left edge (to open), or from anywhere once the
      // drawer is already open (to close it with a left swipe).
      if (!fromLeftEdge && !props.sidebarOpen) return;
      tracking = true;
      horizontal = false;
      startX = t.clientX;
      startY = t.clientY;
      startId = t.identifier;
    };

    const onTouchMove = (e: TouchEvent) => {
      if (!tracking) return;
      const t = Array.from(e.touches).find((x) => x.identifier === startId);
      if (!t) return;
      const dx = t.clientX - startX;
      const dy = t.clientY - startY;
      if (!horizontal) {
        // Hand the gesture back to scrolling when vertical movement wins by a
        // clear margin — otherwise we'd fight the list's own scroll.
        if (Math.abs(dy) > 10 && Math.abs(dy) > Math.abs(dx)) {
          tracking = false;
          return;
        }
        // Engage only once the drag is clearly horizontal.
        if (Math.abs(dx) > 10 && Math.abs(dx) > Math.abs(dy)) {
          horizontal = true;
        } else {
          return;
        }
      }
      // Claim the gesture so the WebView doesn't treat the horizontal drag as a
      // native scroll / edge navigation. Vertical scrolls never reach here, so
      // native scrolling in the lists is unaffected.
      if (e.cancelable) e.preventDefault();
      if (Math.abs(dx) >= THRESHOLD) {
        if (dx > 0) props.setSidebarOpen(true);
        else if (props.sidebarOpen) props.setSidebarOpen(false);
        tracking = false;
        horizontal = false;
      }
    };

    const onTouchEnd = () => {
      tracking = false;
      horizontal = false;
    };

    window.addEventListener("touchstart", onTouchStart, { passive: true });
    // Non-passive move so a confirmed horizontal edge swipe can preventDefault.
    window.addEventListener("touchmove", onTouchMove, { passive: false });
    window.addEventListener("touchend", onTouchEnd, { passive: true });
    window.addEventListener("touchcancel", onTouchEnd);
    onCleanup(() => {
      window.removeEventListener("touchstart", onTouchStart);
      window.removeEventListener("touchmove", onTouchMove);
      window.removeEventListener("touchend", onTouchEnd);
      window.removeEventListener("touchcancel", onTouchEnd);
    });
  });

  // Scheduled runs are job-owned threads (`kind = 'scheduled'`), rendered in
  // their own collapsible section. Everything else is a normal chat.
  // Memoized: these used to re-filter on every render, which meant a new array
  // per render for `<For>` to diff.
  // A pinned chat is rendered once, in its own section at the top, and excluded
  // from the ordinary list (so it never appears twice).
  const pinnedChats = createMemo(() =>
    props.visibleConversations.filter((c) => c.kind !== "scheduled" && c.pinned),
  );
  const chats = createMemo(() =>
    props.visibleConversations.filter((c) => c.kind !== "scheduled" && !c.pinned),
  );
  const scheduled = createMemo(() =>
    props.visibleConversations.filter((c) => c.kind === "scheduled"),
  );
  const [scheduledOpen, setScheduledOpen] = createSignal(true);

  const row = (c: Conversation) => (
    <li class="group relative">
      <Show
        when={props.renameId !== c.id}
        fallback={
          <input
            class="w-full rounded-lg border border-neutral-400 bg-white px-3 py-2 text-sm outline-none dark:border-neutral-600 dark:bg-neutral-800"
            value={props.renameValue}
            onInput={(e) => props.setRenameValue(e.currentTarget.value)}
            onBlur={() => void props.commitRename(c.id)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void props.commitRename(c.id);
              if (e.key === "Escape") props.setRenameId(null);
            }}
          />
        }
      >
        <button
          onClick={() => {
            props.setActiveId(c.id);
            props.setConfirmDeleteId(null);
            props.setSidebarOpen(false);
          }}
          class={`w-full rounded-lg px-3 py-2 pr-20 text-left text-sm transition ${
            props.activeId === c.id
              ? "bg-neutral-200/80 font-medium dark:bg-neutral-800 dark:text-neutral-100"
              : "text-neutral-700 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
          }`}
        >
          {/* The glyph must survive a long title, so truncation moved
              from the button to the inner title span. */}
          <span class="flex min-w-0 items-center gap-1">
            <span class="truncate">{c.title}</span>
            <Show when={props.unseenConversationIds.has(c.id)}>
              <span
                title="New activity"
                aria-label="New activity"
                class="ml-0.5 h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500"
              />
            </Show>
            <Show when={props.cronConversationIds.has(c.id)}>
              <span
                title="Has a scheduled job"
                class="shrink-0 text-neutral-400 dark:text-neutral-500"
              >
                <ClockIcon class="h-3.5 w-3.5" />
              </span>
            </Show>
          </span>
        </button>
        {/* Row actions: hover-revealed at `lg`; always visible on
            narrow (touch) viewports where there is no hover. */}
        <div class="absolute right-1.5 top-1/2 flex -translate-y-1/2 gap-0.5 lg:hidden lg:group-hover:flex">
          <Show when={c.kind !== "scheduled"}>
            <button
              title={c.pinned ? "Unpin chat" : "Pin chat"}
              aria-label={c.pinned ? "Unpin chat" : "Pin chat"}
              aria-pressed={!!c.pinned}
              class={`rounded px-1.5 py-1 transition hover:bg-neutral-200 dark:hover:bg-neutral-800 ${
                c.pinned
                  ? "text-amber-600 dark:text-amber-400"
                  : "text-neutral-400 hover:text-neutral-700 dark:text-neutral-500 dark:hover:text-neutral-200"
              }`}
              onClick={() => props.onTogglePin(c)}
            >
              <svg
                viewBox="0 0 24 24"
                class="h-3.5 w-3.5"
                fill={c.pinned ? "currentColor" : "none"}
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <path d="M12 17v5" />
                <path d="M9 3h6l-1 6 3 3v2H7v-2l3-3-1-6z" />
              </svg>
            </button>
          </Show>
          <button
            title="Rename"
            class="rounded px-1.5 py-1 text-xs text-neutral-400 transition hover:bg-neutral-200 hover:text-neutral-700 dark:text-neutral-500 dark:hover:bg-neutral-800 dark:hover:text-neutral-200"
            onClick={() => {
              props.setRenameId(c.id);
              props.setRenameValue(c.title);
            }}
          >
            ✎
          </button>
          <Show
            when={props.confirmDeleteId === c.id}
            fallback={
              <button
                title={
                  c.kind === "scheduled"
                    ? "Delete this thread and its scheduled job"
                    : "Delete"
                }
                class="rounded px-1.5 py-1 text-xs text-neutral-400 transition hover:bg-red-100 hover:text-red-600 dark:text-neutral-500 dark:hover:bg-red-950 dark:hover:text-red-400"
                onClick={() => props.setConfirmDeleteId(c.id)}
              >
                ✕
              </button>
            }
          >
            <button
              title={
                c.kind === "scheduled"
                  ? "Delete the thread and its job"
                  : "Confirm delete"
              }
              class="rounded bg-red-600 px-1.5 py-1 text-xs text-white"
              onClick={() => void props.removeConversation(c.id)}
            >
              Delete?
            </button>
          </Show>
        </div>
      </Show>
    </li>
  );

  return (
    <>
      {/* Sidebar backdrop — narrow viewports only, closes the drawer on tap.
          Always mounted and faded (150 ms): mounting it only while open made it
          pop in instantly next to the panel, which read as a jolt. The panel
          itself does not animate (see <aside>), so nothing here can shimmer its
          text. `pointer-events-none` when closed keeps it inert, and opacity is
          not a layout property, so there is no reflow. */}
      <div
        class={`fixed inset-0 z-30 bg-black/40 transition-opacity duration-150 ease-out motion-reduce:transition-none lg:hidden ${
          props.sidebarOpen ? "opacity-100" : "pointer-events-none opacity-0"
        }`}
        onClick={() => props.setSidebarOpen(false)}
        aria-hidden="true"
      />
      {/* Sidebar — a slide-in drawer below `lg`, always in flow at `lg` and up.
          The panel itself does NOT animate: a transform transition on a fixed,
          bordered panel makes WebKitGTK shimmer the text for its duration (the
          "jitter" users see on desktop windows narrower than `lg`, where this
          drawer is used). It appears/disappears instantly; only the scrim fades,
          which has no text and cannot shimmer. Nothing here transitions a layout
          property, so the chat column never reflows. */}
      <aside
        id="app-sidebar"
        inert={drawerHidden()}
        aria-hidden={drawerHidden() ? "true" : undefined}
        class={`fixed inset-y-0 left-0 z-40 flex w-72 shrink-0 flex-col border-r border-neutral-200 bg-neutral-50 lg:static lg:z-auto lg:translate-x-0 dark:border-neutral-800 dark:bg-neutral-900 ${
          props.sidebarOpen ? "translate-x-0" : "-translate-x-full"
        }`}
        style={{
          "padding-top": `${props.insets.top}px`,
          "padding-bottom": `${props.insets.bottom}px`,
        }}
      >
        <div class="flex items-center gap-2 border-b border-neutral-200 p-3 dark:border-neutral-800">
          <span class="mr-auto flex items-center gap-2 pl-1">
            <MarkAvatar class="h-5 w-5 shrink-0" />
            <span class="text-sm font-semibold tracking-tight">Pi Chat</span>
          </span>

          <button
            onClick={() => {
              props.createNewChat();
              props.setSidebarOpen(false);
            }}
            title="New chat"
            class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
          >
            + New
          </button>
          <button
            onClick={() => {
              props.setSidebarOpen(false);
              props.onClosed();
            }}
            title="Close chats"
            aria-label="Close chats"
            class="rounded-md p-1.5 text-neutral-500 transition hover:bg-neutral-200 lg:hidden dark:text-neutral-400 dark:hover:bg-neutral-800"
          >
            <CloseIcon />
          </button>
        </div>

        <div class="border-b border-neutral-200 px-3 py-2 dark:border-neutral-800">
          <input
            class="w-full rounded-lg border border-neutral-300 bg-white px-3 py-1.5 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
            placeholder="Search chats…"
            value={props.searchQuery}
            onInput={(e) => props.setSearchQuery(e.currentTarget.value)}
          />
        </div>

        <nav class="flex-1 overflow-y-auto px-2 pb-2 pt-2 [scrollbar-gutter:stable]">
          {/* Pinned chats: always at the top, in their own section, so a long
              history cannot push them out of reach. */}
          <Show when={pinnedChats().length > 0}>
            <div>
              <p class="px-3 pb-1 pt-1 text-[10px] font-semibold uppercase tracking-wide text-neutral-400 dark:text-neutral-500">
                Pinned
              </p>
              <ul class="space-y-1">
                <For each={pinnedChats()}>{(c) => row(c)}</For>
              </ul>
              <div class="my-2 border-t border-neutral-200 dark:border-neutral-800" />
            </div>
          </Show>
          <ul class="space-y-1">
            <For each={chats()}>{(c) => row(c)}</For>
            <Show when={props.visibleConversations.length === 0}>
              <li class="px-3 py-2 text-sm text-neutral-400 dark:text-neutral-500">
                No chats found.
              </li>
            </Show>
          </ul>
          {/* Scheduled runs live on job-owned threads. They get their own
              section so a job firing every minute never pushes real chats
              down the list (and so a run's output is never mistaken for a
              conversation the user started). */}
          <Show when={scheduled().length > 0}>
            <div class="mt-3 border-t border-neutral-200 pt-2 dark:border-neutral-800">
              <button
                onClick={() => setScheduledOpen((v) => !v)}
                title={scheduledOpen() ? "Collapse scheduled runs" : "Expand scheduled runs"}
                class="flex w-full items-center gap-1.5 rounded-md px-3 py-1 text-[11px] font-semibold uppercase tracking-wide text-neutral-400 transition hover:bg-neutral-100 hover:text-neutral-600 dark:text-neutral-500 dark:hover:bg-neutral-800 dark:hover:text-neutral-300"
              >
                <span
                  class={`inline-block transition-transform ${scheduledOpen() ? "rotate-90" : ""}`}
                  aria-hidden="true"
                >
                  ›
                </span>
                <ClockIcon class="h-3 w-3" />
                Scheduled
                {/* Collapsed rows hide their own dots, so surface unseen
                    scheduled output on the section header too. */}
                <Show when={scheduled().some((c) => props.unseenConversationIds.has(c.id))}>
                  <span
                    title="New activity in a scheduled thread"
                    aria-label="New activity in a scheduled thread"
                    class="h-1.5 w-1.5 shrink-0 rounded-full bg-amber-500"
                  />
                </Show>
                <span class="ml-auto font-normal normal-case tracking-normal">
                  {scheduled().length}
                </span>
              </button>
              <Show when={scheduledOpen()}>
                <ul class="mt-1 space-y-1">
                  <For each={scheduled()}>{(c) => row(c)}</For>
                </ul>
              </Show>
            </div>
          </Show>
        </nav>
      </aside>
    </>
  );
}
