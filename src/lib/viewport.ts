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
 * **Android also has the native bridge.** `MainActivity.kt` publishes the IME
 * (and system-bar) inset through `window.PiInsets` + the `pi-insets` event, and
 * App.tsx pads the app root by that amount. Android WebView consumes the IME
 * insets natively, so the visual viewport shrinks by the keyboard height while
 * `innerHeight` stays at the full layout height. The two must never be ADDED:
 * App.tsx pads `<main>` by `max(0, estimate - nativeBottom)` and follows
 * `max(estimate, nativeBottom)` for the transcript. On a healthy bridge the
 * native inset wins (extra = 0); on a bridge that reports nothing (an OEM
 * WebView, or `< API 30` where the value depends on `adjustResize`) the estimate
 * is the only lift, so the composer still rises.
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

  // Every input below can fire in bursts (a keyboard animates open, focus
  // moves, the page scrolls); measure once per frame rather than per event.
  let frame = 0;
  const measure = () => {
    frame = 0;
    // `offsetTop` matters when the visual viewport is scrolled (some mobile
    // browsers pan the page behind the keyboard); without it the inset can
    // over- or under-count. `height === 0` means "not measured yet" (hidden
    // page, teardown), not a fully covered viewport — report no lift then.
    const covered = Math.max(0, window.innerHeight - vv.height - vv.offsetTop);
    setInset(vv.height > 0 ? covered : 0);
  };
  const update = () => {
    if (frame) return;
    frame = requestAnimationFrame(measure);
  };

  vv.addEventListener("resize", update);
  vv.addEventListener("scroll", update);
  // `focusin`/`focusout` cover browsers that open the keyboard on focus but do
  // not emit a prompt visualViewport resize; `resize`/`pageshow` cover an
  // orientation change or a back-forward-cache restore; `orientationchange`
  // is the older signal some WebViews still rely on.
  window.addEventListener("resize", update);
  window.addEventListener("focusin", update);
  window.addEventListener("focusout", update);
  window.addEventListener("pageshow", update);
  window.addEventListener("orientationchange", update);
  onCleanup(() => {
    if (frame) cancelAnimationFrame(frame);
    vv.removeEventListener("resize", update);
    vv.removeEventListener("scroll", update);
    window.removeEventListener("resize", update);
    window.removeEventListener("focusin", update);
    window.removeEventListener("focusout", update);
    window.removeEventListener("pageshow", update);
    window.removeEventListener("orientationchange", update);
  });

  update();

  return inset;
}
