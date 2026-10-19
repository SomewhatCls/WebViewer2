const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (s) => document.querySelector(s);
const HOME = "https://duckduckgo.com";
const CLOSE_SVG = '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>';

// Web Store detail pages look like /detail/<slug>/<32-letter id>
const storeId = (url) =>
  /^https:\/\/chromewebstore\.google\.com\/detail\/[^/]+\/([a-p]{32})/.exec(url ?? "")?.[1] ?? null;
// Our own pages are served from tauri.localhost; show a friendly address.
const shown = (url) => (/^https?:\/\/tauri\.localhost\/extensions\.html/.test(url ?? "") ? "about:extensions" : url ?? "");

const tabs = new Map(); // id -> { title, url, loading }
let active = null;
let seq = 0;

async function openTab(url = HOME) {
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

listen("tab-updated", ({ payload: p }) => {
  const t = tabs.get(p.id);
  if (!t) return;
  if (p.url != null) {
    t.url = p.url;
    if (t.title === "New tab") {
      try { t.title = new URL(p.url).hostname; } catch {}
    }
  }
  if (p.title) t.title = p.title;
  if (p.loading != null) t.loading = p.loading;
  render();
});

listen("open-tab", ({ payload: url }) => openTab(url));

const nav = (action) => active && invoke("nav", { id: active, action });
$("#back").onclick = () => nav("back");
$("#fwd").onclick = () => nav("forward");
$("#reload").onclick = () => nav("reload");
$("#new").onclick = () => openTab();
$("#ext").onclick = () => openTab("about:extensions");

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

const addr = $("#addr");
addr.onfocus = () => addr.select();
addr.onkeydown = (e) => {
  if (e.key === "Enter" && active && addr.value.trim()) {
    if (addr.value.trim() === "about:extensions") openTab("about:extensions");
    else invoke("navigate", { id: active, input: addr.value });
    addr.blur();
  } else if (e.key === "Escape") {
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

openTab();
