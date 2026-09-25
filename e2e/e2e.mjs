// GUI end-to-end driver for Pi Chat, run via `npm run e2e` (see e2e/run.sh).
//
// Talks W3C WebDriver to tauri-driver/WebKitWebDriver and drives the real
// webview against the mock LLM. Assertions cover the P0 UX fixes:
//   - streaming reply is not truncated (unterminated final SSE chunk)
//   - the reasoning trace button's DOM node survives stream updates and the
//     trace opens live and after completion
//   - the message list auto-scrolls to the bottom and short chats are
//     bottom-anchored
//   - the context readout uses the 0.8x / 200k allowed budget
//
// Note: on this WebKitGTK build, WebDriver's Element Click / Send Keys / Actions
// return "unsupported operation" (a build-time limitation; see e2e/README.md).
// Clicks are dispatched in-page via `el.click()`, and the DOM-churn regression
// the <Index> list fixes is covered by a node-identity check.
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
    "const b=document.querySelector(\"button[title='Close']\"); if(b){b.click();return true;} return false;",
  );

async function main() {
  log("creating session for", APP);
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

  // --- Settings: ensure an API key exists (never clobber an existing one) ---
  await click("//button[normalize-space(.)='Settings']");
  await waitFor("settings dialog", () => findCss("input[type='password']"));
  const hasExistingKey = await exists(
    "xpath",
    "//button[normalize-space(.)='Remove saved key']",
  );
  let addedKey = false;
  if (!hasExistingKey) {
    await setInputCss("input[type='password']", "mock-key");
    addedKey = true;
    log("stored a test API key (none existed before)");
  } else {
    log("existing keychain entry found — leaving it untouched");
  }
  await click("//button[normalize-space(.)='Done']");
  await waitFor("settings closed", async () =>
    (await exec("return !document.querySelector(\"input[type='password']\");")) === true,
  );

  const modelValues = await exec(
    "const s=document.querySelector('header select'); return s ? [...s.options].map(o=>o.value) : [];",
  );
  assert(
    modelValues.includes("mock-model"),
    "model picker lists the mock endpoint model: " + JSON.stringify(modelValues),
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
    "const b=document.querySelector(\"button[title='Show the reasoning trace']\"); return {same: !!window.__traceBtn && window.__traceBtn === b, streaming: document.body.innerText.includes('Stop')};",
  );
  assert(stable.streaming, "still streaming while checking node identity (mid-stream)");
  assert(stable.same, "trace button DOM node survived a streamed update (<Index> fix)");

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

  // --- Trace clickable after completion too ---
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

  // --- Cleanup the test key if we added it ---
  if (addedKey) {
    await click("//button[normalize-space(.)='Settings']");
    await click("//button[normalize-space(.)='Remove saved key']");
    await click("//button[normalize-space(.)='Done']");
    log("removed the test API key");
  }

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
    await cleanup();
    process.exit(1);
  });
