# GUI end-to-end test

Drives the real Pi Chat webview (via `tauri-driver` + `WebKitWebDriver`) against
a local mock OpenAI-compatible SSE server, asserting the streaming/UX behavior
that can't be covered by unit tests.

```bash
npm run e2e
```

Screenshots land in `e2e/screenshots/` (gitignored). On failure, `99-failure.png`
is written and the visible page text is dumped.

## Prerequisites

- `tauri-driver` v2.0.6 (Tauri v2): `cargo install tauri-driver --version 2.0.6 --locked`
- A debug build (`src-tauri/target/debug/pi-chat`) — built automatically if missing
- A desktop session; the app window opens on the current display

## What it covers

| Assertion | Mechanism |
| --- | --- |
| Reply isn't truncated | The mock's final SSE event omits its terminating blank line; the reply must still contain `FINALTAIL` |
| Reasoning trace survives streaming | The trace button's DOM node identity is checked across a streamed update (`<Index>` fix), then opened and asserted to show `(live)` |
| Trace after completion | Reopened and asserted to contain the reasoning |
| Auto-scroll | `scrollHeight - scrollTop - clientHeight <= 80px` after a turn |
| Bottom anchoring | empty chat's list wrapper computes `justify-content: flex-end` |
| Context budget | model picker lists the mock model; header uses the 0.8×/200k cap |

## How it works

- `run.sh` seeds an isolated `XDG_DATA_HOME` with a `config.json` pointing at the
  mock (`http://127.0.0.1:8317/v1`) and a `mock-model` id, then starts the mock,
  Vite, and `tauri-driver`, and runs `e2e.mjs`.
- `mock-llm.mjs` serves `/v1/models` and a scripted `/v1/chat/completions` SSE
  stream (reasoning → content → usage → unterminated final content).
- `e2e.mjs` is a tiny W3C WebDriver client (node `fetch`, no deps).

## Why clicks use `el.click()` (not WebDriver input)

On this WebKitGTK build, WebDriver's `Element Click`, `Send Keys`, and W3C
**Actions** commands return `unsupported operation`. The `/usr/bin/WebKitWebDriver`
driver ships from the **webkitgtk6.0** (GTK4) package, while a Tauri app runs on
**webkit2gtk-4.1** (GTK3); the browser-side input path isn't wired for that
pairing, and there's no runtime flag or capability to enable it (`browserName`
makes no difference). This is a long-standing Tauri-on-Linux issue
([tauri#6541](https://github.com/tauri-apps/tauri/issues/6541)); the accepted
workaround is `browser.execute('arguments[0].click()')`, which is what we do.

Because a script click can't reproduce a mouse-down/up landing on different DOM
nodes, the click-churn regression is covered by the **node-identity assertion**
instead: the trace button's DOM node is captured, a streamed delta is allowed to
land, and the same node object must still be there. (`<For>` recreated it;
`<Index>` keeps it.)

Genuinely trusted input would require a WebKitGTK built with the WebDriver input
backend enabled (or OS-level XTEST/`xdotool`) — not worth the rebuild cost here.
An isolated-display XTEST harness was prototyped and removed as overengineering.

## Notes / limitations

- The API key: the harness stores a throwaway `mock-key` **only if the keychain
  has no entry**, and removes it afterward. An existing key is left untouched.
- Env knobs: `MOCK_PORT`, `WD_PORT`, `APP`, `OUT`.
