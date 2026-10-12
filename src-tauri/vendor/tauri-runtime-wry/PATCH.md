# Vendored: `tauri-runtime-wry` 2.11.4

This crate is a verbatim copy of `tauri-runtime-wry` **2.11.4** from crates.io,
with a single upstream bug fix applied. It is wired in through
`[patch.crates-io]` in `src-tauri/Cargo.toml`.

## Why

Upstream 2.11.4 emits the app-level `RunEvent::Resumed` only in its
`Event::NewEvents(StartCause::Poll)` arm. The event loop drives
`ControlFlow::Wait`, so that arm never fires — `RunEvent::Resumed` is dead on
every platform. On Android the resume *is* delivered internally as
`Event::Resumed`, but the `#[cfg(mobile)]` branch that consumes it only
dispatches per-window events (`WindowEvent::Resumed`), which are dropped when
no window exists.

That is precisely the state Pi Chat lands in after the user swipes the app away:
`RunEvent::ExitRequested` is answered with `api.prevent_exit()` and
`KeepAliveService` keeps the process (and its Rust schedulers) alive, so no
window survives. A relaunch resumes with zero windows, our `lib.rs`
`RunEvent::Resumed` handler (which would recreate the window) never runs, and the
activity comes back blank until the user force-stops the process.

## The patch

Upstream PR [tauri-apps/tauri#15678](https://github.com/tauri-apps/tauri/pull/15678),
fixes tauri#15671. Applied to `src/lib.rs` in `handle_event_loop`, in the
`#[cfg(mobile)] e @ Event::Resumed | e @ Event::Suspended` arm:

```diff
     #[cfg(mobile)]
     e @ Event::Resumed | e @ Event::Suspended => {
+      // Surface the resume to the app-level callback as well. The per-window
+      // dispatch below never runs when there are no windows (e.g. on Android
+      // when the activity was destroyed while a foreground service kept the
+      // process alive), so the app would otherwise never learn about it.
+      if matches!(e, Event::Resumed) {
+        callback(RunEvent::Resumed);
+      }
+
       let event = match e {
         Event::Resumed => WindowEvent::Resumed,
         Event::Suspended => WindowEvent::Suspended,
```

The rest of PR #15678 also touches `tauri-runtime` (doc comment only) and
`tauri/src/app.rs` (re-creates configured windows from inside the runtime). We
deliberately do **not** vendor those crates; our own `lib.rs` handles window
recreation so the changed surface stays as small as possible.

## Maintenance

When a released `tauri-runtime-wry` > 2.11.4 includes the fix:

1. Remove the `[patch.crates-io]` entry from `src-tauri/Cargo.toml`.
2. Delete this directory.
3. Delete the `RunEvent::Resumed` recreate handler in `src-tauri/src/lib.rs`
   only if the upstream `tauri` crate is also bumped to the release that
   re-creates windows itself (PR #15678's `app.rs` change); otherwise keep it.
