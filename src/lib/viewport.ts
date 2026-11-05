import { createSignal, onCleanup, type Accessor } from "solid-js";

/**
 * How far the on-screen keyboard covers the layout viewport, in CSS pixels.
 *
 * Mobile browsers resize the *visual* viewport (not the layout viewport) when
 * the soft keyboard opens, so `window.innerHeight` still reports the full
 * height while `visualViewport` reports what is actually visible. The keyboard
 * occupies the difference between the layout viewport and the visual viewport
 * (minus any scroll offset of the visual viewport itself).
 *
 * Call inside a component/root: the listeners are released via `onCleanup`.
 * Returns 0 on platforms without `visualViewport` (desktop), so the caller's
 * padding collapses to nothing and the layout is unchanged.
 */
export function createKeyboardInset(): Accessor<number> {
  const [inset, setInset] = createSignal(0);
  if (typeof window === "undefined") return inset;

  const vv = window.visualViewport;
  if (!vv) return inset;

  const update = () => {
    // `offsetTop` matters when the visual viewport is scrolled (some Android
    // keyboards pan the page); without it the inset can over- or under-count.
    setInset(Math.max(0, window.innerHeight - vv.height - vv.offsetTop));
  };

  update();
  vv.addEventListener("resize", update);
  vv.addEventListener("scroll", update);
  window.addEventListener("orientationchange", update);
  onCleanup(() => {
    vv.removeEventListener("resize", update);
    vv.removeEventListener("scroll", update);
    window.removeEventListener("orientationchange", update);
  });

  return inset;
}
