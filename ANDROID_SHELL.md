# ANDROID_SHELL — Shell via the user's Termux

Status: **implemented (desktop core + Android bridge scaffold)**. The one-shot
executor (desktop) replaces the persistent shell and is fully unit-tested; the
Android `RUN_COMMAND` bridge (Rust `crate::android` + Kotlin `RunCommandPlugin`)
is wired so the Rust path compiles for the aarch64 Android target, but the
Termux round-trip needs **on-device validation** (no emulator/device in this
environment; see Testing).
Scope: how Pi Chat's `bash` / `read_file` / `write_file` tools behave on Android.
Note: written to the Plan dir because Plan mode blocks writes outside it;
intended final location is `~/chat/ANDROID_SHELL.md`.

## TL;DR

No embedded/packaged userland. On Android, the shell tools execute **inside the
user's installed Termux** through Termux's public `RUN_COMMAND` intent. One
coherent environment, working directory on **visible shared storage** so the
agent works directly on the user's real files. The persistent per-conversation
shell is **removed** (on desktop too) — every tool call is a one-shot execution.

## Decision log

| Decision | Choice | Why |
| --- | --- | --- |
| Persistent per-conversation shell | **Removed (desktop + Android)** | Agent already assumes non-continuous state; kills the shell-manager machinery and its tests |
| How to get a shell on Android | **Use the user's installed Termux** via `RUN_COMMAND` intent | Lowest resistance; no packaging, no exec/linker/W^X work, no update treadmill (Termux self-updates) |
| Embedded userland / proot | **Rejected** | Packaging, updates, exec-permission and prefix-baking scope creep |
| Server-side VM | **Future idea only, out of scope** | Transfers (and grows) the burden into ops/security/accounts; not low-resistance |
| `read_file` / `write_file` | **Route through Termux** (bash-backed one-liners, same gate) | One coherent filesystem namespace; no own Android storage layer / SAF engineering |
| Working directory | **Shared storage (user-visible files)** | Agent works directly on the user's real files |
| `HOME` vs `cwd` | **Kept identical** (one directory) | No split-brain; single coherent environment |
| Canonical paths | `/storage/emulated/0/...` | Same path the user sees in a file manager; real path inside Termux bash |

Out of scope for this plan: server VM, embedded userland, proot, a
Pi Chat–owned Android storage layer, container/sandbox tool execution.

## Why this works at all (key Android constraints)

- An app **cannot access another app's data dir** (per-app UID + SELinux
  `app_data_file`; no permission grants cross-app access). Pi Chat cannot read
  Termux's `/data/data/com.termux/files/...`.
- Therefore every shell-side action must be **executed on Termux's side** via
  the intent — the command runs in Termux's process and is allowed to touch
  Termux's files.
- `/storage/emulated/0/...` is a **real path** reachable by Termux's bash once
  Termux holds broad shared-storage access. It is the one namespace both the
  user (file manager) and the agent (shell) can agree on.

## Architecture

```
SolidJS (unchanged UI)
   │  invoke("run_tool", ...)   [existing tool loop / approval gate]
   ▼
src-tauri  tools.rs  ─  one-shot executor (replaces shell.rs ShellRegistry)
   ├─ desktop:  tokio Command::new("bash").args(["-c", cmd]).output()
   └─ android:  → Android plugin bridge (gen/android, Kotlin/Java)
                      │
                      ▼
              Termux RUN_COMMAND Intent
                 (com.termux.permission.RUN_COMMAND)
                      │  PendingIntent → BroadcastReceiver
                      ▼
              Result Bundle (stdout / stderr / exit code) → Rust → frontend
```

### One-shot executor (new, both platforms)

Replaces `src-tauri/src/shell.rs` `ShellRegistry` / `ShellSession` entirely.
Interface: `async fn run(conversation_id, command) -> ToolOutput` becomes a
plain fire-and-forget execution; `conversation_id` is no longer needed for
session state. The sentinel line, heredoc redirect, and read-until-timeout
logic are deleted (captured output replaces them).

- **Desktop:** `Command::new("bash").arg("-c").arg(cmd).output()`, capture
  `stdout`/`stderr`, map exit code to `ToolOutput { content, is_error }`.
- **Android:** delegate to the bridge below.
- Both platforms share one behavior contract: fresh process, fresh env/cwd per
  call, no persistence.

### Android bridge (`gen/android`, Kotlin/Java)

Follow the pattern Tauri itself uses for mobile plugins
(`register_android_plugin` in `tauri-plugin-shell` is a reference), exposed to
Rust as a `tauri::command`:

- **Feature-detect:** `PackageManager.getPackageInfo("com.termux", 0)`
  (presence + version; result receipt requires Termux ≥ 0.109).
- **Send:** build
  `Intent("com.termux.RUN_COMMAND")` → `RunCommandService` with
  `EXTRA_COMMAND_PATH = $PREFIX/bin/bash`, `EXTRA_ARGUMENTS = ["-c", cmd]`,
  `EXTRA_WORKDIR = <workspace>`, `EXTRA_STDIN` (optional), and a **unique**
  `executionId`-scoped result `PendingIntent`.
- **Receive:** a `BroadcastReceiver` that reads the result Bundle
  (`stdout`/`stderr`/exit code) and resolves a per-call `oneshot::channel`
  consumed by the Rust command.
- **Permissions:** `uses-permission com.termux.permission.RUN_COMMAND` in the
  manifest; UI flows for granting it (App Info → Additional Permissions), plus
  guidance to enable `allow-external-apps` in Termux's `termux.properties`.

### File tools (`read_file` / `write_file`)

Kept in the tool surface and approval-gated as today, but backed by Termux
one-liners so they share the same namespace as `bash`:

- `read_file(path)`  → `cat <path>` (with a size cap).
- `write_file(path, content)` → heredoc / `tee` (path-validated).
- Absolute canonical paths (`/storage/emulated/0/...`) used directly.

### Environment / working directory

- **HOME == cwd == one working directory** on shared storage, by default a
  dedicated visible workspace (configurable setting), e.g.
  `/storage/emulated/0/PiChat/`. Created on first run.
  - Rationale: keeping `HOME` = the raw `/storage/emulated/0` root would scatter
    dotfiles/`~/.ssh`/config across the device; a workspace dir keeps `~` tidy
    while still living in user-visible shared storage.
- Per-call `WORKDIR` points at this directory. `PATH`/`LD_PRELOAD`/`PREFIX`
  inherited from Termux's environment (Termux sets these up).
- The agent navigates to any user file via absolute `/storage/emulated/0/...`
  path, so "works directly on user's files" is preserved.

### Storage permission / onboarding

Because execution runs in Termux, the broad shared-storage grant lives with
**Termux**, not Pi Chat:

- Guide the user once through `termux-setup-storage` / granting Termux
  "All files access" (MANAGE_EXTERNAL_STORAGE). Note: Play-restricted, but fine
  for direct/F-Droid distribution consistent with the app's ethos.
- **Feature-detect & degrade:** if Termux is absent → `bash` returns
  "Termux not installed"; if storage isn't granted → `bash` still works but
  confined to Termux's private home, with a prompt to grant storage.

## Implementation steps

1. **Mobile target:** `npm run tauri android init` (none existed today). ✅ — the app now has `src-tauri/gen/android`; the Rust crate compiles for the aarch64 Android target (`cargo check --target aarch64-linux-android`).
2. **One-shot executor:** `src-tauri/src/shell.rs` replaced with a thin one-shot
   executor (`ShellExecutor`); the persistence/sentinel logic and its tests were
   deleted. `tools.rs` / `chat.rs` / `reflection.rs` / `db.rs` / `lib.rs` rewired;
   the `bash` test suite was rewritten for fresh-env semantics. ✅ — desktop unit
   tests pass (105 passed).
3. **Android bridge:** Rust `crate::android` (`run_command` Tauri plugin →
   `run_termux_command`) + Kotlin `RunCommandPlugin` in `gen/android` + the
   `com.termux.permission.RUN_COMMAND` manifest permission, with feature-detect
   (Termux presence) and result routing. 🔶 — Rust path compiles for Android;
   Kotlin is written to the documented Termux/RUN_COMMAND API but the real
   Termux round-trip needs on-device validation.
4. **File tools:** `read_file`/`write_file` route through the executor — native
   filesystem on desktop, Termux one-liners on Android. ✅
5. **Permission & onboarding UI:** settings entry for the working directory and
   guidance text (Termux setup / storage grant / degraded-state messaging). ✅ / 🔶
   (on-device flow still to be exercised).
6. **Docs:** updated `AGENTS.md` (tool surface + platform notes) and this file. ✅

## Security / approval

- Per-call approval gate for `bash`/`read_file`/`write_file` stays in front of
  the intent (model never executes ungated).
- Working on real user files raises blast radius — the approval prompt should
  surface the target path(s), and destructive commands warrant clear display.
- Cross-app boundary is respected: only Termux already holds the grants; Pi
  Chat never requests broad storage access itself.

## Risks / gotchas

- **Termux version drift / intent changes** — pin the documented API, feature-
  detect, and fail loudly with guidance.
- **Result size limits** — the `PendingIntent` result Bundle is bounded; very
  large outputs may truncate. Mitigate with output caps and/or have the agent
  write large output to a file and read it back.
- **Scoped storage carve-outs** — `Android/data` & `Android/obb` remain
  off-limits even with All-files-access.
- **Fresh-env semantics regression** — desktop users lose persistent
  `cd`/`export`; acceptable (agent assumes non-persistence), but worth a
  release note.
- **OEM behavior** — Termux execution and `RUN_COMMAND` edge cases vary per
  vendor/Android version; test on a matrix where feasible.

## Testing

- Unit tests for the one-shot executor on desktop (output, exit codes, missing
  bash).
- Android bridge: not covered by the existing `npm run e2e` (that's desktop
  WebKit); add a mock/gradle-side test for intent construction + result
  parsing, and manual device testing for the real Termux round-trip.
- Degraded states: Termux absent, version too old, storage not granted.

## Open knobs (configurable, defaulted)

- Working-directory default path under shared storage (default
  `/storage/emulated/0/PiChat`).
- Whether `bash`/file tools are present-but-degraded vs hidden when Termux is
  missing (recommended: present, returning a clear "set up Termux" message).
- Output size cap for `RUN_COMMAND` results.
