import { createEffect, createSignal, For, onMount, Show } from "solid-js";
import { Dialog } from "@kobalte/core/dialog";
import {
  createConversation,
  listConversations,
  listMessages,
  type Conversation,
  type Message,
} from "./lib/api";

export default function App() {
  const [conversations, setConversations] = createSignal<Conversation[]>([]);
  const [activeId, setActiveId] = createSignal<string | null>(null);
  const [messages, setMessages] = createSignal<Message[]>([]);
  const [draftTitle, setDraftTitle] = createSignal("");
  const [open, setOpen] = createSignal(false);

  onMount(async () => {
    const rows = await listConversations();
    setConversations(rows);
    if (rows.length > 0) setActiveId(rows[0].id);
  });

  // Auto-load messages whenever the active conversation changes.
  createEffect(() => {
    const id = activeId();
    if (!id) {
      setMessages([]);
      return;
    }
    listMessages(id).then(setMessages);
  });

  async function createNewChat() {
    const created = await createConversation(draftTitle() || undefined);
    setConversations([created, ...conversations()]);
    setActiveId(created.id);
    setDraftTitle("");
    setOpen(false);
  }

  const activeTitle = () => {
    const id = activeId();
    return conversations().find((c) => c.id === id)?.title ?? "New chat";
  };

  return (
    <div class="flex h-screen w-screen overflow-hidden bg-white text-neutral-900">
      {/* Sidebar */}
      <aside class="flex w-72 shrink-0 flex-col border-r border-neutral-200 bg-neutral-50">
        <div class="flex items-center justify-between gap-2 border-b border-neutral-200 p-3">
          <span class="pl-1 text-sm font-semibold tracking-tight">Pi Chat</span>

          <Dialog.Root open={open()} onOpenChange={setOpen}>
            <Dialog.Trigger class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700">
              + New
            </Dialog.Trigger>
            <Dialog.Portal>
              <Dialog.Overlay class="fixed inset-0 z-50 bg-black/40" />
              <Dialog.Content class="fixed left-1/2 top-1/2 z-50 w-full max-w-sm -translate-x-1/2 -translate-y-1/2 rounded-xl border border-neutral-200 bg-white p-5 shadow-xl focus:outline-none">
                <Dialog.Title class="text-base font-semibold">New chat</Dialog.Title>
                <Dialog.Description class="mt-1 text-sm text-neutral-500">
                  Give the conversation a name (optional).
                </Dialog.Description>
                <input
                  class="mt-4 w-full rounded-lg border border-neutral-300 px-3 py-2 text-sm outline-none focus:border-neutral-500"
                  placeholder="Untitled chat"
                  value={draftTitle()}
                  onInput={(e) => setDraftTitle(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") createNewChat();
                  }}
                />
                <div class="mt-4 flex justify-end gap-2">
                  <Dialog.CloseButton class="rounded-md px-3 py-1.5 text-sm text-neutral-600 transition hover:bg-neutral-100">
                    Cancel
                  </Dialog.CloseButton>
                  <button
                    onClick={() => createNewChat()}
                    class="rounded-md bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-neutral-700"
                  >
                    Create
                  </button>
                </div>
              </Dialog.Content>
            </Dialog.Portal>
          </Dialog.Root>
        </div>

        <nav class="flex-1 overflow-y-auto px-2 pb-2 pt-2">
          <ul class="space-y-1">
            <For each={conversations()}>
              {(c) => (
                <li>
                  <button
                    onClick={() => setActiveId(c.id)}
                    class={`w-full truncate rounded-lg px-3 py-2 text-left text-sm transition ${
                      activeId() === c.id
                        ? "bg-neutral-200/80 font-medium"
                        : "text-neutral-700 hover:bg-neutral-100"
                    }`}
                  >
                    {c.title}
                  </button>
                </li>
              )}
            </For>
          </ul>
        </nav>
      </aside>

      {/* Main */}
      <main class="flex min-w-0 flex-1 flex-col">
        <header class="flex items-center justify-between border-b border-neutral-200 px-6 py-3">
          <h1 class="truncate text-sm font-semibold">{activeTitle()}</h1>
          <span class="shrink-0 text-xs text-neutral-400">Model — configure in Milestone 3</span>
        </header>

        <section class="flex-1 overflow-y-auto px-6 py-4">
          <Show
            when={activeId()}
            fallback={<p class="text-sm text-neutral-400">Select or create a conversation.</p>}
          >
            <div class="space-y-3">
              <For each={messages()}>
                {(m) => (
                  <div class="flex justify-start">
                    <div
                      class={`max-w-[80%] rounded-xl px-4 py-2.5 text-sm ${
                        m.role === "assistant" ? "bg-neutral-100" : "bg-neutral-900 text-white"
                      }`}
                    >
                      <div class="mb-0.5 text-[11px] opacity-60">{m.role}</div>
                      <div class="whitespace-pre-wrap break-words">{m.content}</div>
                    </div>
                  </div>
                )}
              </For>
              <Show when={messages().length === 0}>
                <p class="text-sm text-neutral-400">No messages yet.</p>
              </Show>
            </div>
          </Show>
        </section>

        <footer class="border-t border-neutral-200 p-4">
          <textarea
            class="w-full resize-none rounded-xl border border-neutral-300 px-4 py-3 text-sm outline-none focus:border-neutral-500"
            rows={3}
            placeholder="Message… (streaming wiring is Milestone 2)"
          />
        </footer>
      </main>
    </div>
  );
}
