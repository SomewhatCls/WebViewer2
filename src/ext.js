const { invoke } = window.__TAURI__.core;
const $ = (s) => document.querySelector(s);
async function load() {
  const exts = await invoke("list_extensions");
  const list = $("#list");
  list.replaceChildren();
  if (!exts.length) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No extensions yet. Open an extension on the Chrome Web Store and select Add to browser.";
    list.append(li);
  }
  for (const e of exts) {
    const li = document.createElement("li");
    const text = document.createElement("div");
    text.className = "ext-text";
    const name = document.createElement("strong");
    name.textContent = e.name;
    const meta = document.createElement("span");
    meta.textContent = `${e.version} · ${e.active ? "On" : "Starts after restart"}`;
    text.append(name, meta);
    const rm = document.createElement("button");
    rm.className = "subtle";
    rm.textContent = "Remove";
    rm.onclick = async () => {
      try { await invoke("remove_extension", { id: e.id }); $("#msg").textContent = `Removed ${e.name}.`; }
      catch (err) { $("#msg").textContent = String(err); }
      load();
    };
    li.append(text, rm);
    list.append(li);
  }
  $("#banner").hidden = !exts.some((e) => !e.active);
}

$("#addform").onsubmit = async (ev) => {
  ev.preventDefault();
  const input = $("#src").value.trim();
  if (!input) return;
  $("#msg").textContent = "Adding…";
  try {
    const e = await invoke("install_extension", { input });
    $("#msg").textContent = `Added ${e.name}.` + (e.active ? " Reload open tabs to use it." : "");
    $("#src").value = "";
  } catch (err) {
    $("#msg").textContent = String(err);
  }
  load();
};

$("#restart").onclick = () => invoke("restart_app");
load();
