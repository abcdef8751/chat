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
 * **Android uses the native bridge instead.** `MainActivity.kt` publishes the
 * IME (and system-bar) inset through `window.PiInsets` + the `pi-insets` event,
 * and App.tsx pads the app root by that amount. That native inset is
 * authoritative whenever it reports a non-zero bottom inset: Android WebView
 * consumes the IME insets natively, so the visual viewport shrinks by the
 * keyboard height while `innerHeight` stays at the full layout height.
 * Subtracting them there would report the keyboard height a SECOND time, and
 * App.tsx would pad the app root *and* <main> — the composer gets pushed twice
 * as far up and the transcript shrinks twice. Never re-add the two. The
 * estimate is only returned while the bridge reports **no** bottom inset at all
 * (a WebView that never delivered one), as a last-resort fallback.
 *
 * Call inside a component/root: the listeners are released via `onCleanup`.
 * Returns 0 on platforms without `visualViewport` (desktop), so the caller's
 * padding collapses to nothing and the layout is unchanged.
 */
export function createKeyboardInset(): Accessor<number> {
  const [inset, setInset] = createSignal(0);
  if (typeof window === "undefined") return inset;

  // The native shell owns the IME on Android (see above). It is authoritative
  // whenever it reports a non-zero bottom inset (the nav bar when the keyboard
  // is closed, the IME when it is open). If it reports 0 it has delivered
  // nothing — some OEM WebViews and the `< API 30` path can behave that way —
  // and this estimate is the only chance to lift the composer, so it is kept as
  // a last-resort fallback rather than disabling the helper outright.
  const native = (
    window as unknown as {
      PiInsets?: { getBottom?: () => number };
    }
  ).PiInsets;
  const nativeOwnsBottom = () => {
    if (!native) return false;
    try {
      return (native.getBottom?.() ?? 0) > 0;
    } catch {
      // A disposed JavascriptInterface throws; treat it as "no native value".
      return false;
    }
  };

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
    setInset(vv.height > 0 && !nativeOwnsBottom() ? covered : 0);
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
