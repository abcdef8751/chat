import { invoke } from "@tauri-apps/api/core";
import type { Attachment } from "./api";

/**
 * Android platform bridge.
 *
 * Wraps the pieces of the native layer that only exist on Android so callers
 * never poke at `window` globals directly. The native side is
 * `src-tauri/gen/android/.../MainActivity.kt`:
 *
 * - `PiInsets` — system-bar insets (`getTop()` … `getRight()`).
 * - `PiBack`   — `exit()` finishes the activity (JS→native).
 * - `pi-back`  — a cancelable DOM event the native back handler dispatches.
 *
 * On desktop every value here is inert: `isAndroid` is `false`, `onBack`
 * registers nothing, `exitApp` does nothing, and `readAttachmentFromUri`
 * rejects (the native command is Android-only).
 */

interface InsetsBridge {
  getTop(): number;
  getBottom(): number;
  getLeft(): number;
  getRight(): number;
}

interface BackBridge {
  /** Finish the activity (JS→native). */
  exit(): void;
}

interface AndroidWindow extends Window {
  PiInsets?: InsetsBridge;
  PiBack?: BackBridge;
}

/**
 * True iff the native Android JS interface is present. `PiInsets` is injected
 * via `addJavascriptInterface` in `onWebViewCreate`, so it is absent on
 * desktop (and in a plain browser/dev server).
 */
export const isAndroid: boolean =
  typeof window !== "undefined" && !!(window as AndroidWindow).PiInsets;

/**
 * Subscribe to the native hardware/gesture back press. `cb` returns `true`
 * when it consumed the press (closed a dialog, popped the drawer) and `false`
 * when it did not, in which case the native side finishes the activity.
 * Returns an unsubscribe function.
 *
 * The native side dispatches a cancelable `pi-back` event and inspects
 * `defaultPrevented`, so "handled" is expressed by calling `preventDefault()`.
 */
export function onBack(cb: () => boolean): () => void {
  if (typeof window === "undefined") return () => {};
  const handler = (event: Event) => {
    if (cb()) event.preventDefault();
  };
  window.addEventListener("pi-back", handler);
  return () => window.removeEventListener("pi-back", handler);
}

/** Ask Android to finish the activity. No-op on desktop. */
export function exitApp(): void {
  if (typeof window === "undefined") return;
  (window as AndroidWindow).PiBack?.exit();
}

/**
 * Read an attachment from an Android `content://` URI (what the dialog plugin
 * returns there, and what `std::fs` cannot open). The Kotlin
 * `ContentResolver` reads the bytes; Rust assembles the same `Attachment`
 * shape as the path-based `readAttachments`. Android only — on desktop the
 * command returns an error.
 */
export function readAttachmentFromUri(uri: string): Promise<Attachment> {
  return invoke<Attachment>("read_attachment_from_uri", { uri });
}
