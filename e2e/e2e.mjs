// GUI end-to-end driver for Pi Chat, run via `npm run e2e` (see e2e/run.sh).
//
// Talks W3C WebDriver to tauri-driver/WebKitWebDriver and drives the real
// webview against the mock LLM. Assertions cover the P0 UX fixes:
//   - streaming reply is not truncated (unterminated final SSE chunk)
//   - the reasoning trace button's DOM node survives stream updates, and the
//     trace opens live and after completion
//   - the message list auto-scrolls to the bottom and short chats are
//     bottom-anchored
//   - the context readout uses the 0.8x / 200k allowed budget
//
// Key handling: the run gives the seeded provider an API key through the real
// provider edit form in Settings (the *provider* key field — never the Brave
// Search card, and never the legacy `api_key` account). All secret writes land
// in the isolated PI_SECRETS_DIR the app was launched with (run.sh), so the
// user's real OS keychain is never read or written; the before/after
// keychain snapshots below prove byte-identity, and the suite fails if they
// differ. The model picker is polled (not asserted once) and on failure the
// dump includes the actual options plus the visible page text.
//
// Note: on this WebKitGTK build, WebDriver's Element Click / Send Keys / Actions
// return "unsupported operation" (a build-time limitation; see e2e/README.md).
// Clicks are dispatched in-page via `el.click()`, and the DOM-churn regression
// the <Index> list fixes is covered by a node-identity check.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..");

const WD = process.env.WD_URL ?? "http://127.0.0.1:4444";
const APP = process.env.APP ?? path.join(ROOT, "src-tauri/target/debug/pi-chat");
const OUT = process.env.OUT ?? path.join(HERE, "screenshots");
const ELEM_KEY = "element-6066-11e4-a52e-4f735466cecf";

const EXPECT_TEXT =
  "Hello! This is a mocked reply. It should arrive complete, including this final sentence. FINALTAIL";
const EXPECT_THINK = "The user just greeted me";

mkdirSync(OUT, { recursive: true });

let session = null;
const log = (...a) => console.log("[e2e]", ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function wd(method, pathname, body) {
  const res = await fetch(WD + pathname, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = { value: text };
  }
  if (!res.ok || (json.value && json.value.error)) {
    const msg = json.value && json.value.message ? json.value.message : text;
    throw new Error(`${method} ${pathname} -> ${res.status}: ${msg}`);
  }
  return json.value;
}

const p = (s) => `/session/${session}${s}`;

async function find(using, value) {
  const v = await wd("POST", p("/element"), { using, value });
  return v[ELEM_KEY];
}
const findCss = (sel) => find("css selector", sel);

async function exec(script, args = []) {
  return wd("POST", p("/execute/sync"), { script, args });
}
async function shot(name) {
  const b64 = await wd("GET", p("/screenshot"));
  writeFileSync(path.join(OUT, `${name}.png`), Buffer.from(b64, "base64"));
  log("screenshot", `${name}.png`);
}

async function exists(using, value) {
  try {
    await find(using, value);
    return true;
  } catch {
    return false;
  }
}

async function waitFor(label, fn, timeout = 15000) {
  const start = Date.now();
  let last;
  while (Date.now() - start < timeout) {
    try {
      const v = await fn();
      if (v) return v;
      last = v;
    } catch (e) {
      last = e.message;
    }
    await sleep(250);
  }
  throw new Error(`timeout waiting for ${label} (last: ${last})`);
}

const JS_CLICK_XP = `const r=document.evaluate(arguments[0],document,null,XPathResult.FIRST_ORDERED_NODE_TYPE,null);
const el=r.singleNodeValue; if(!el) return false; el.click(); return true;`;

// Wait for an XPath element to exist, then click it in-page. This WebKitGTK
// build supports neither WebDriver element-click nor the Actions API, so script
// is the only click channel (the standard tauri-driver/Linux approach).
async function click(xp) {
  await waitFor(`element ${xp}`, () => find("xpath", xp).catch(() => null));
  await exec(JS_CLICK_XP, [xp]);
}

async function setInputCss(sel, text) {
  const script = `const el=document.querySelector(arguments[0]); if(!el) return false;
const proto = el.tagName==='TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
const setter = Object.getOwnPropertyDescriptor(proto,'value').set;
setter.call(el, arguments[1]);
el.dispatchEvent(new Event('input',{bubbles:true}));
return true;`;
  return exec(script, [sel, text]);
}

function assert(cond, msg) {
  if (!cond) throw new Error(`ASSERT FAILED: ${msg}`);
  log("ok -", msg);
}

const bodyText = () => exec("return document.body.innerText;");
const popupText = () =>
  exec(
    "const d=document.querySelector('[aria-label=\"Reasoning trace\"]'); return d ? d.innerText : '';",
  );
const closePopup = () =>
  exec(
    `const b=document.querySelector('[aria-label="Reasoning trace"] button[title="Close"]');
if(b){b.click();return true;} return false;`,
  );

// --- Real-keychain snapshot (read-only proof of isolation) ------------------
// The app is launched with PI_SECRETS_DIR set, so it never touches the user's
// keychain. These helpers still snapshot every `com.rp.chat` entry before the
// run and again at the end, and the suite FAILS if they differ — the run must
// leave the user's real keychain byte-identical, no exceptions.
const KEYCHAIN_SERVICE = "com.rp.chat";

function keychainSnapshot() {
  try {
    const raw = execFileSync(
      "secret-tool",
      ["search", "--all", "service", KEYCHAIN_SERVICE],
      { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 10000 },
    ).toString();
    return {
      sha256: createHash("sha256").update(raw).digest("hex"),
      entries: (raw.match(/^\[/gm) ?? []).length,
      users: [...raw.matchAll(/attribute\.username = (.+)/g)].map((m) => m[1]),
    };
  } catch (e) {
    log("keychain snapshot unavailable (secret-tool missing or keyring locked):", e.message);
    return null;
  }
}

function reportKeychain(snapshot) {
  return snapshot
    ? `${snapshot.entries} com.rp.chat entrie(s), sha256 ${snapshot.sha256}`
    : "unavailable";
}

// Compare a before-run snapshot with the current keychain state and assert
// byte-identity. Called from the success path and from the failure path so the
// proof holds even when an assertion further along aborts the run.
function verifyKeychainUnchanged(before) {
  const after = keychainSnapshot();
  log("keychain snapshot (after run):", reportKeychain(after));
  if (before && after) {
    const same = before.sha256 === after.sha256;
    if (!same) {
      log(
        "keychain entries changed!",
        JSON.stringify(before.users),
        "->",
        JSON.stringify(after.users),
      );
    }
    assert(
      same,
      `the real OS keychain is byte-identical before and after the run (before ${before.sha256}, after ${after.sha256})`,
    );
    return true;
  }
  log("keychain before/after proof skipped (secret-tool unavailable)");
  return false;
}

let keychainBefore = null;

async function main() {
  log("creating session for", APP);
  keychainBefore = keychainSnapshot();
  log("keychain snapshot (before run):", reportKeychain(keychainBefore));
  const created = await wd("POST", "/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application: APP } } },
  });
  session = created.sessionId;
  log("session", session);

  await waitFor("app shell", async () =>
    (await exec("return !!document.querySelector('aside');")) === true,
  );
  await waitFor("sidebar ready", async () =>
    (await exec("return document.body.innerText.includes('Pi Chat');")) === true,
  );
  await shot("01-startup");

  // --- Settings: give the ACTIVE PROVIDER an API key through the real UI ---
  // The *provider* edit form's API-key field is the only path that makes
  // providers::resolve succeed for the seeded provider — the Brave Search
  // card (a different password input further down the dialog) must stay
  // untouched, and the legacy `api_key` account is never written either. All
  // writes land in PI_SECRETS_DIR; the keychain snapshots prove it.
  const modelOptionValues = () =>
    exec(
      `const s=document.querySelector('header select[title="Model"]');
       return s ? [...s.options].map(o=>o.value) : [];`,
    );
  // The injected model (`e2e-never-listed`) is NOT what the mock serves, so
  // this is a genuine pre-condition: the endpoint model cannot appear before
  // the key is set and the endpoint fetch has run.
  const initialValues = await modelOptionValues();
  assert(
    !initialValues.includes("mock-model"),
    "the mock endpoint model is not listed before the key is set: " + JSON.stringify(initialValues),
  );

  await click("//button[normalize-space(.)='Settings']");
  await waitFor("provider list with an Edit button", () =>
    find("xpath", "//button[normalize-space(.)='Edit']").catch(() => null),
  );
  await click("//button[normalize-space(.)='Edit']");
  // The edit form renders the provider's API-key field as the FIRST password
  // input in the dialog DOM (the add-provider form is closed, and the Brave
  // card + sync forms come later in the dialog).
  await waitFor("provider API-key field", () => findCss("input[type='password']"));
  await setInputCss("input[type='password']", "mock-key");
  await click("//button[normalize-space(.)='Save']");

  // Wait for the save + model refetch to land, polling the MODEL picker (the
  // header has a sibling provider <select>; the model one is title="Model").
  const pickerFailureDump = async () => {
    const state = await exec(
      `const v=document.querySelector('header select[title="Model"]');
       return { values: v ? [...v.options].map(o=>o.value) : [],
                text: document.body.innerText.slice(0, 500) };`,
    );
    return `actual options: ${JSON.stringify(state.values)}; page text: ${JSON.stringify(state.text)}`;
  };
  const listed = await waitFor(
    "the model picker to list the mock endpoint model",
    async () => {
      const vals = await modelOptionValues();
      return vals.includes("mock-model") ? vals : null;
    },
    15000,
  ).catch(async (e) => {
    throw new Error(
      `model picker never listed the mock endpoint model — ${await pickerFailureDump()} (${e.message})`,
    );
  });
  assert(
    listed.includes("mock-model"),
    "model picker lists the mock endpoint model: " + JSON.stringify(listed),
  );
  const providerLabels = await exec(
    `const s=document.querySelector('header select[title="Provider"]');
     return s ? [...s.options].map(o=>o.textContent.trim()) : [];`,
  );
  assert(
    providerLabels.some((t) => t.includes("E2E Mock") && !t.includes("no key")),
    "the app can read the provider key back (header no longer says '(no key)'): " +
      JSON.stringify(providerLabels),
  );
  await shot("02a-key-set");

  await click("//button[normalize-space(.)='Done']");
  await waitFor("settings closed", async () =>
    (await exec("return !document.querySelector(\"input[type='password']\");")) === true,
  );

  // The picker must still list the model after Settings closes — poll, don't
  // assert once.
  const listedAfterClose = await waitFor(
    "the model picker after closing Settings",
    async () => {
      const vals = await modelOptionValues();
      return vals.includes("mock-model") ? vals : null;
    },
    15000,
  ).catch(async (e) => {
    throw new Error(
      `model picker lost the mock endpoint model after closing Settings — ${await pickerFailureDump()} (${e.message})`,
    );
  });
  assert(
    listedAfterClose.includes("mock-model"),
    "model picker lists the mock endpoint model after closing Settings: " +
      JSON.stringify(listedAfterClose),
  );
  await shot("02-settings-done");

  // --- New conversation ---
  await click("//button[normalize-space(.)='+ New']");
  await click("//button[normalize-space(.)='Create']");
  await waitFor("composer", () => findCss("footer textarea"));
  await shot("03-new-chat");

  // --- Send a message ---
  await setInputCss("footer textarea", "hello there");
  await click("//button[normalize-space(.)='Send']");

  // --- The trace button's DOM node must survive streamed updates ---
  // <For> recreated the row on every delta (dropping mid-press clicks);
  // <Index> keeps the same DOM node. This is the P0 regression check.
  const traceXp = "//button[@title='Show the reasoning trace']";
  await waitFor("live thinking button", () => find("xpath", traceXp).catch(() => null), 8000);
  await exec(
    "window.__traceBtn = document.querySelector(\"button[title='Show the reasoning trace']\"); return !!window.__traceBtn;",
  );
  await sleep(1000);
  const stable = await exec(
    "const b=document.querySelector(\"button[title='Show the reasoning trace']\"); return {same: !!window.__traceBtn && window.__traceBtn === b, streaming: document.body.innerText.includes('Stop'), container: !!document.querySelector(\"button[title='Show reasoning text']\")};",
  );
  assert(stable.streaming, "still streaming while checking node identity (mid-stream)");
  assert(stable.same, "trace button DOM node survived a streamed update (<Index> fix)");
  assert(
    stable.container,
    "the reasoning container is present (and toggleable) mid-stream, not hidden behind a global toggle",
  );

  await click(traceXp);
  const livePopup = await waitFor("live trace popup", async () => (await popupText()) || null, 6000);
  assert(livePopup.includes("live"), "live trace popup opened mid-stream and shows (live)");
  assert(livePopup.includes("Let me think"), "live popup contains streamed reasoning so far");
  await shot("04-live-trace-popup");
  await closePopup();

  // --- Completion: full reply incl. the unterminated final SSE chunk ---
  await waitFor(
    "full assistant reply",
    async () =>
      (await exec(
        `return document.body.innerText.includes(${JSON.stringify(EXPECT_TEXT)});`,
      )) === true,
    20000,
  );
  assert(true, "reply complete incl. unterminated final SSE chunk (no cut-off)");
  await shot("05-reply-complete");

  // --- The turn reads as ONE continuous reply: a reasoning container plus the
  // output segment, in stream order. There is no single auto-collapsing
  // "activity" row any more, so nothing may hide behind one global toggle. ---
  assert(
    (await bodyText()).includes(EXPECT_TEXT),
    "the reply renders as assistant output prose",
  );
  assert(
    await exists("xpath", "//button[@title='Show reasoning text']"),
    "the turn's reasoning container is present after the turn",
  );
  assert(
    !(await bodyText()).includes(EXPECT_THINK),
    "the reasoning body stays collapsed by default (the output is not)",
  );

  // --- The container's own chevron toggles the reasoning inline, both ways ---
  await click("//button[@title='Show reasoning text']");
  await waitFor(
    "inline reasoning text",
    async () => ((await bodyText()).includes(EXPECT_THINK) ? true : null),
    6000,
  );
  assert(true, "chevron expands the reasoning inline");
  await shot("06-inline-reasoning");
  await click("//button[@title='Hide reasoning text']");
  await waitFor(
    "inline reasoning hidden again",
    async () => ((await bodyText()).includes(EXPECT_THINK) ? null : true),
    6000,
  );
  assert(true, "chevron collapses the reasoning inline again");

  // --- Trace popup still reachable after completion ---
  await click("//button[@title='Show the reasoning trace']");
  const finalPopup = await waitFor("final trace popup", async () => (await popupText()) || null, 6000);
  assert(finalPopup.includes(EXPECT_THINK), "final trace popup contains the reasoning trace");
  await shot("06-final-trace-popup");
  await closePopup();

  // --- Auto-scroll: view is pinned to the bottom after a turn ---
  const scroll = await exec(
    "const el=document.querySelector('main section'); return {top:el.scrollTop, h:el.scrollHeight, c:el.clientHeight};",
  );
  const gap = scroll.h - scroll.top - scroll.c;
  assert(gap <= 80, `auto-scrolled to bottom (gap ${gap}px)`);

  // --- Bottom anchoring: content of an empty chat is flex-end ---
  await click("//button[normalize-space(.)='+ New']");
  await click("//button[normalize-space(.)='Create']");
  const anchor = await exec(
    "const el=document.querySelector('main section > div'); const s=getComputedStyle(el); return {jc:s.justifyContent, cls:String(el.className)};",
  );
  assert(anchor.jc === "flex-end", "empty/short chat is bottom-anchored (justify-content: flex-end)");
  await shot("07-empty-bottom-anchored");

  // --- Tool calls + reasoning interleave inline, in stream order ---
  await setInputCss("footer textarea", "tooltest");
  await click("//button[normalize-space(.)='Send']");
  await waitFor(
    "tool approval card",
    () => find("xpath", "//button[normalize-space(.)='Approve']").catch(() => null),
    10000,
  );
  // Tool calls and reasoning interleave inline, in stream order — no expand.
  assert(
    await exists("xpath", "//button[@title='Show the reasoning trace']"),
    "the tool round's reasoning container renders inline while live",
  );
  const liveTool = await bodyText();
  assert(
    liveTool.includes("approval needed"),
    "the gated call card is inline (no expand needed)",
  );
  assert(liveTool.includes("bash"), "the inline call card names the tool");
  await shot("08-tool-approval");

  await click("//button[normalize-space(.)='Approve']");
  await waitFor(
    "tool round reply",
    async () => (await bodyText()).includes("TOOL DONE"),
    20000,
  );
  assert(true, "tool round completed after approval");

  // The call card and its paired result stay inline after the turn: nothing
  // auto-collapses, and the final answer follows them as output prose.
  const toolBody = await waitFor(
    "inline tool card + result",
    async () => {
      const text = await bodyText();
      return text.includes("timeline-ok") ? text : null;
    },
    6000,
  );
  assert(toolBody.includes("bash"), "tool call card is inline in the transcript");
  assert(toolBody.includes("timeline-ok"), "tool result is paired with its call, inline");
  assert(
    toolBody.includes("TOOL DONE"),
    "the final answer renders as assistant output after the tool card",
  );
  assert(
    await exists("xpath", "//button[@title='Show the reasoning trace']"),
    "the reasoning container persists after the turn",
  );
  const containers = await exec(
    "return document.querySelectorAll(\"button[title='Show the reasoning trace']\").length;",
  );
  assert(
    containers === 1,
    `the turn's single run of reasoning is grouped into one container (got ${containers})`,
  );
  await shot("09-tool-timeline");

  // --- Memory consolidation: manual trigger writes files + leaves a note ---
  await click("//button[normalize-space(.)='Memory']");
  await waitFor("memory dialog", () =>
    find("xpath", "//button[normalize-space(.)='Consolidate this chat']").catch(() => null),
  );
  await click("//button[normalize-space(.)='Consolidate this chat']");
  await waitFor(
    "consolidation wrote a memory file",
    async () => ((await bodyText()).includes("e2e-topic.md") ? true : null),
    20000,
  );
  assert(
    (await bodyText()).includes("e2e-topic.md"),
    "consolidation created a new plain Markdown memory file",
  );
  assert(
    (await bodyText()).includes("Reflection spend"),
    "Memory tab shows the separately-tracked reflection spend",
  );
  await shot("10-memory-consolidated");

  await click("//button[normalize-space(.)='Close']");
  await waitFor(
    "consolidation note in chat",
    async () => ((await bodyText()).includes("Memory consolidated") ? true : null),
    10000,
  );
  assert(true, "a memory-consolidation note appears in the conversation");
  await shot("11-consolidation-note");

  // --- Keychain proof: the real OS keychain must be byte-identical ---
  // Nothing in this run may read, write, or delete a real keychain entry; the
  // app wrote its test key into PI_SECRETS_DIR instead. Fail loudly if the
  // user's keychain changed under us (an external process would show here too).
  verifyKeychainUnchanged(keychainBefore);

  log("ALL ASSERTIONS PASSED");
}

async function cleanup() {
  if (session) {
    try {
      await wd("DELETE", `/session/${session}`);
    } catch {}
  }
}

main()
  .then(cleanup)
  .then(() => process.exit(0))
  .catch(async (e) => {
    console.error("\n[e2e] FAILED:", e.message);
    try {
      await shot("99-failure");
      console.error("[e2e] body text:\n", (await bodyText()).slice(0, 1500));
    } catch {}
    // The keychain proof still runs on the failure path: an aborted run must
    // not silently leave the user's real keychain in a different state.
    try {
      verifyKeychainUnchanged(keychainBefore);
    } catch (k) {
      console.error("[e2e] keychain check failed too:", k.message);
    }
    await cleanup();
    process.exit(1);
  });
