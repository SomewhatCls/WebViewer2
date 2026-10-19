const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s) => document.querySelector(s);
const CLOSE_SVG = '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>';
const INTERNAL = new Set(["about:extensions", "about:settings"]);

// Mirrors the Rust `Settings`; replaced by the saved values at startup.
let settings = { theme: "system", search: "duckduckgo", home: "https://duckduckgo.com", save_history: true };

function applyTheme() {
  const t = settings.theme;
  if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
  else delete document.documentElement.dataset.theme;
}

// Web Store detail pages look like /detail/<slug>/<32-letter id>
const storeId = (url) =>
  /^https:\/\/chromewebstore\.google\.com\/detail\/[^/]+\/([a-p]{32})/.exec(url ?? "")?.[1] ?? null;
// Our own pages are served from tauri.localhost; show a friendly address.
const shown = (url) => {
  const m = /^https?:\/\/tauri\.localhost\/(extensions|settings)\.html/.exec(url ?? "");
  return m ? `about:${m[1]}` : url ?? "";
};

const tabs = new Map(); // id -> { title, url, loading }
let active = null;
let seq = 0;

async function openTab(url = settings.home) {
  const id = "t" + ++seq;
  tabs.set(id, { title: "New tab", url, loading: true });
  active = id;
  render();
  try {
    await invoke("new_tab", { id, url });
  } catch (e) {
    tabs.get(id).title = "Failed to open";
    tabs.get(id).loading = false;
    render();
  }
}

// Internal pages (settings, extensions) reuse an already-open tab.
function openInternal(url) {
  const found = [...tabs].find(([, t]) => t.url === url || shown(t.url) === url);
  found ? activate(found[0]) : openTab(url);
}

function activate(id) {
  active = id;
  render();
  invoke("activate_tab", { id });
}

function closeTab(id) {
  const ids = [...tabs.keys()];
  const idx = ids.indexOf(id);
  tabs.delete(id);
  invoke("close_tab", { id });
  if (active === id) {
    const next = [...tabs.keys()][Math.min(idx, tabs.size - 1)];
    next ? activate(next) : openTab();
  }
  render();
}

function render() {
  const root = $("#tabs");
  root.replaceChildren();
  for (const [id, t] of tabs) {
    const el = document.createElement("div");
    el.className = "tab" + (id === active ? " active" : "");
    el.setAttribute("role", "tab");
    el.title = t.title;

    const title = document.createElement("span");
    title.className = "title";
    title.textContent = t.title;

    const x = document.createElement("button");
    x.className = "close icon";
    x.setAttribute("aria-label", "Close tab");
    x.innerHTML = CLOSE_SVG;
    x.onclick = (e) => { e.stopPropagation(); closeTab(id); };

    el.append(title, x);
    el.onclick = () => activate(id);
    el.onauxclick = (e) => { if (e.button === 1) closeTab(id); };
    root.append(el);
  }
  const t = tabs.get(active);
  $("#progress").classList.toggle("on", !!t?.loading);
  if (document.activeElement !== $("#addr")) $("#addr").value = shown(t?.url);
  $("#add").hidden = !storeId(t?.url);
}

// Adds a finished page load to the history (the backend ignores internal pages and dedupes).
function record(t) {
  if (!settings.save_history || t.loading) return;
  let u;
  try { u = new URL(t.url); } catch { return; }
  if (!/^https?:$/.test(u.protocol) || u.hostname === "tauri.localhost") return;
  invoke("history_add", { url: t.url, title: t.title === "New tab" ? "" : t.title });
}

listen("tab-updated", ({ payload: p }) => {
  const t = tabs.get(p.id);
  if (!t) return;
  // Safety net: if a bounce to Chrome's new tab page slips past the backend, step back once.
  if (p.url?.startsWith("chrome-search://")) {
    if (!t.bounced) { t.bounced = true; invoke("nav", { id: p.id, action: "back" }); }
    return;
  }
  if (p.url != null) {
    t.url = p.url;
    if (t.title === "New tab") {
      try { t.title = new URL(p.url).hostname; } catch {}
    }
  }
  if (p.title) t.title = p.title;
  if (p.loading != null) t.loading = p.loading;
  render();
  // Record when a load finishes, and again if the title arrives afterwards.
  if (p.loading === false || (p.title && !t.loading)) record(t);
});

listen("open-tab", ({ payload: url }) => (INTERNAL.has(url) ? openInternal(url) : openTab(url)));
listen("settings-changed", ({ payload }) => { settings = payload; applyTheme(); });

const nav = (action) => active && invoke("nav", { id: active, action });
$("#back").onclick = () => nav("back");
$("#fwd").onclick = () => nav("forward");
$("#reload").onclick = () => nav("reload");
$("#new").onclick = () => openTab();
$("#ext").onclick = () => openInternal("about:extensions");
$("#settings").onclick = () => openInternal("about:settings");

$("#add").onclick = async () => {
  const btn = $("#add");
  const id = storeId(tabs.get(active)?.url);
  if (!id) return;
  btn.disabled = true;
  btn.textContent = "Adding…";
  try {
    await invoke("install_extension", { input: id });
    openTab("about:extensions"); // shows the restart prompt
  } catch (e) {
    btn.textContent = "Couldn’t add";
    btn.title = String(e);
    await new Promise((r) => setTimeout(r, 2500));
  }
  btn.disabled = false;
  btn.textContent = "Add to browser";
  btn.title = "";
};

// ---------- Address-bar suggestions (from the history) ----------
// The UI webview is only 84px tall, so while the list is open it grows by the list's height
// and the page webviews slide down by the same amount (see `set_suggest_height` in main.rs).
const ROW_H = 40, PAD = 8;
const SEARCH_SVG = '<svg viewBox="0 0 24 24"><circle cx="10.5" cy="10.5" r="6.5"/><path d="M15.5 15.5 21 21"/></svg>';
const PAGE_SVG = '<svg viewBox="0 0 24 24"><path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4"/></svg>';
let rows = [];
let sel = -1;
let suggestTimer = 0;
let suggestSeq = 0;

function closeSuggest() {
  clearTimeout(suggestTimer);
  suggestSeq++; // drop any answer still on its way
  if (!rows.length) return;
  rows = [];
  sel = -1;
  $("#suggest").hidden = true;
  invoke("set_suggest_height", { height: 0 });
}

function paintSuggest() {
  const box = $("#suggest");
  box.replaceChildren();
  rows.forEach((r, i) => {
    const el = document.createElement("div");
    el.className = "srow" + (i === sel ? " sel" : "");
    el.setAttribute("role", "option");

    const ico = document.createElement("span");
    ico.className = "sico";
    ico.innerHTML = r.query != null ? SEARCH_SVG : PAGE_SVG;

    const main = document.createElement("span");
    main.className = "smain";
    if (r.query != null) {
      main.textContent = r.query;
    } else {
      let host = r.url;
      try { host = new URL(r.url).hostname.replace(/^www\./, ""); } catch {}
      const t = document.createElement("span");
      t.className = "stitle";
      t.textContent = r.title || host;
      const u = document.createElement("span");
      u.className = "surl";
      u.textContent = host;
      main.append(t, u);
    }
    el.append(ico, main);
    // mousedown (not click) so the address box keeps focus and its blur does not close the list first
    el.onmousedown = (e) => { e.preventDefault(); pick(r); };
    box.append(el);
  });
}

function pick(r) {
  addr.value = r.url;
  closeSuggest();
  if (active) invoke("navigate", { id: active, input: r.url });
  addr.blur();
}

async function updateSuggest() {
  const q = addr.value.trim();
  const seq = ++suggestSeq;
  if (!q || INTERNAL.has(q)) return closeSuggest();
  let list = [];
  try { list = await invoke("history_suggest", { query: q }); } catch {}
  if (seq !== suggestSeq || document.activeElement !== addr) return; // stale answer
  if (!list.length) { suggestSeq++; return closeSuggest(); }
  rows = list;
  sel = -1;
  paintSuggest();
  $("#suggest").hidden = false;
  invoke("set_suggest_height", { height: rows.length * ROW_H + PAD });
}

const addr = $("#addr");
addr.onfocus = () => addr.select();
addr.oninput = () => { clearTimeout(suggestTimer); suggestTimer = setTimeout(updateSuggest, 60); };
addr.onblur = () => closeSuggest();
addr.onkeydown = (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    if (!rows.length) return;
    e.preventDefault();
    const n = rows.length;
    sel = e.key === "ArrowDown" ? (sel + 1) % n : (sel - 1 + n) % n;
    paintSuggest();
  } else if (e.key === "Enter" && active && addr.value.trim()) {
    if (sel >= 0 && rows[sel]) return pick(rows[sel]);
    const v = addr.value.trim();
    closeSuggest();
    if (INTERNAL.has(v)) openInternal(v);
    else invoke("navigate", { id: active, input: addr.value });
    addr.blur();
  } else if (e.key === "Escape") {
    closeSuggest();
    addr.blur();
    render();
  }
};

// Shortcuts work while the toolbar has focus; page focus needs native handling.
window.addEventListener("keydown", (e) => {
  if (!e.ctrlKey) return;
  const k = e.key.toLowerCase();
  if (k === "t") { e.preventDefault(); openTab(); }
  else if (k === "w") { e.preventDefault(); active && closeTab(active); }
  else if (k === "l") { e.preventDefault(); addr.focus(); }
  else if (k === ",") { e.preventDefault(); openInternal("about:settings"); }
});

// Custom window frame: drag the empty strip, double-click to maximize.
const MAX_SVG = '<svg viewBox="0 0 10 10"><path d="M.5.5h9v9h-9z"/></svg>';
const RESTORE_SVG = '<svg viewBox="0 0 10 10"><path d="M2.5 2.5v-2h7v7h-2M.5 2.5h7v7h-7z"/></svg>';
$("#strip").addEventListener("mousedown", (e) => {
  if (e.button !== 0 || e.target.closest(".tab, button")) return;
  invoke(e.detail === 2 ? "win_toggle_maximize" : "win_drag");
});
$("#win-min").onclick = () => invoke("win_minimize");
$("#win-max").onclick = () => invoke("win_toggle_maximize");
$("#win-close").onclick = () => invoke("win_close");
async function syncMax() {
  const max = await invoke("win_is_maximized");
  const b = $("#win-max");
  b.innerHTML = max ? RESTORE_SVG : MAX_SVG;
  b.title = max ? "Restore" : "Maximize";
  b.setAttribute("aria-label", b.title);
}
window.addEventListener("resize", syncMax); // the UI webview resizes with the window
syncMax();

(async () => {
  try { settings = await invoke("get_settings"); } catch {}
  applyTheme();
  // Mica is only available on Windows 11; without it the strip keeps a solid colour.
  try { if (await invoke("has_mica")) document.documentElement.classList.add("mica"); } catch {}
  openTab();
})();
