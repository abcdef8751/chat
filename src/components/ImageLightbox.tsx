import { createSignal, onCleanup, onMount, Show, type JSX } from "solid-js";

/// The currently expanded image, if any. Module-level so any transcript surface
/// can call `openImage()` without threading a signal through props; the single
/// `<ImageLightbox />` mounted in `App` renders whatever is here.
const [current, setCurrent] = createSignal<{ src: string; alt: string } | null>(null);

/// Expand `src` centred on screen. `alt` doubles as the caption (attachment
/// names, or the markdown image's alt text).
export function openImage(src: string, alt?: string): void {
  if (!src) return;
  setCurrent({ src, alt: alt?.trim() ?? "" });
}

export function closeImage(): void {
  setCurrent(null);
}

/// One centered overlay for expanded transcript images, using the same idiom as
/// the reasoning popup: backdrop click, Escape and ✕ all close it.
export function ImageLightbox(): JSX.Element {
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && current()) closeImage();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <Show when={current()}>
      {(img) => (
        <div
          class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-6"
          onClick={closeImage}
          role="dialog"
          aria-modal="true"
          aria-label={img().alt || "Image preview"}
        >
          <button
            onClick={closeImage}
            title="Close"
            aria-label="Close"
            class="absolute right-4 top-4 rounded-md bg-black/50 px-2 py-1 text-sm text-white transition hover:bg-black/70"
          >
            ✕
          </button>
          <div
            class="flex max-h-[90vh] max-w-[90vw] flex-col items-center gap-2"
            onClick={(e) => e.stopPropagation()}
          >
            <img
              src={img().src}
              alt={img().alt}
              title={img().alt || undefined}
              class="max-h-[90vh] max-w-[90vw] rounded-lg object-contain shadow-xl"
            />
            <Show when={img().alt}>
              <p class="max-w-[90vw] truncate rounded-md bg-black/60 px-2 py-1 text-xs text-white">
                {img().alt}
              </p>
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
}
