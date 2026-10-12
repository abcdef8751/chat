import { createEffect, For, onCleanup, Show } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import type { Conversation } from "../lib/api";
import { CloseIcon, MarkAvatar } from "../lib/icons";

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
  draftTitle: string;
  setDraftTitle: (value: string) => void;
  newOpen: boolean;
  setNewOpen: (open: boolean) => void;
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

  return (
    <>
      {/* Sidebar backdrop — narrow viewports only, closes the drawer on tap */}
      <Show when={props.sidebarOpen}>
        <div
          class="fixed inset-0 z-30 bg-black/40 lg:hidden"
          onClick={() => props.setSidebarOpen(false)}
          aria-hidden="true"
        />
      </Show>
      {/* Sidebar — a slide-in drawer below `lg`, always in flow at `lg` and up */}
      <aside
        id="app-sidebar"
        inert={drawerHidden()}
        aria-hidden={drawerHidden() ? "true" : undefined}
        class={`fixed inset-y-0 left-0 z-40 flex w-72 shrink-0 flex-col border-r border-neutral-200 bg-neutral-50 transition-transform lg:static lg:z-auto lg:translate-x-0 lg:transition-none dark:border-neutral-800 dark:bg-neutral-900 ${
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

          <Dialog open={props.newOpen} onOpenChange={props.setNewOpen}>
            <Dialog.Trigger class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300">
              + New
            </Dialog.Trigger>
            <Dialog.Portal>
              <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
              <Dialog.Content class="fixed left-1/2 top-1/2 z-50 w-full max-w-sm -translate-x-1/2 -translate-y-1/2 rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none dark:border-neutral-700 dark:bg-neutral-900">
                <Dialog.Title class="text-base font-semibold">New chat</Dialog.Title>
                <Dialog.Description class="mt-1 text-sm text-neutral-500 dark:text-neutral-400">
                  Give the conversation a name (optional).
                </Dialog.Description>
                <input
                  class="mt-4 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500 dark:border-neutral-700 dark:bg-neutral-800"
                  placeholder="Untitled chat"
                  value={props.draftTitle}
                  onInput={(e) => props.setDraftTitle(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") props.createNewChat();
                  }}
                />
                <div class="mt-4 flex justify-end gap-2">
                  <Dialog.CloseButton class="rounded-md px-3 py-1.5 text-sm text-neutral-600 transition hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800">
                    Cancel
                  </Dialog.CloseButton>
                  <button
                    onClick={() => props.createNewChat()}
                    class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700 dark:bg-white dark:text-neutral-900 dark:hover:bg-neutral-300"
                  >
                    Create
                  </button>
                </div>
              </Dialog.Content>
            </Dialog.Portal>
          </Dialog>
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

        <nav class="flex-1 overflow-y-auto px-2 pb-2 pt-2">
          <ul class="space-y-1">
            <For each={props.visibleConversations}>
              {(c) => (
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
                      class={`w-full truncate rounded-lg px-3 py-2 pr-14 text-left text-sm transition ${
                        props.activeId === c.id
                          ? "bg-neutral-200/80 font-medium dark:bg-neutral-800 dark:text-neutral-100"
                          : "text-neutral-700 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                      }`}
                    >
                      {c.title}
                    </button>
                    {/* Row actions: hover-revealed at `lg`; always visible on
                        narrow (touch) viewports where there is no hover. */}
                    <div class="absolute right-1.5 top-1/2 flex -translate-y-1/2 gap-0.5 lg:hidden lg:group-hover:flex">
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
                            title="Delete"
                            class="rounded px-1.5 py-1 text-xs text-neutral-400 transition hover:bg-red-100 hover:text-red-600 dark:text-neutral-500 dark:hover:bg-red-950 dark:hover:text-red-400"
                            onClick={() => props.setConfirmDeleteId(c.id)}
                          >
                            ✕
                          </button>
                        }
                      >
                        <button
                          title="Confirm delete"
                          class="rounded bg-red-600 px-1.5 py-1 text-xs text-white"
                          onClick={() => void props.removeConversation(c.id)}
                        >
                          Delete?
                        </button>
                      </Show>
                    </div>
                  </Show>
                </li>
              )}
            </For>
            <Show when={props.visibleConversations.length === 0}>
              <li class="px-3 py-2 text-sm text-neutral-400 dark:text-neutral-500">
                No chats found.
              </li>
            </Show>
          </ul>
        </nav>
      </aside>
    </>
  );
}
