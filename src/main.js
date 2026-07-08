// Launcher UI logic. Talks to the Rust backend through the global Tauri API
// (enabled by `withGlobalTauri`). When opened in a plain browser (design
// preview) it falls back to an in-memory mock so the UI still renders.
function makeMockInvoke() {
  let store =
    JSON.parse(localStorage.getItem("ca_mock") || "null") || [
      { id: "1", name: "Work", gradient: 0, created_at: 0, last_used: Math.floor(Date.now() / 1000) - 3600 },
      { id: "2", name: "Personal", gradient: 1, created_at: 0, last_used: 0 },
      { id: "3", name: "Client — Northwind", gradient: 2, created_at: 0, last_used: Math.floor(Date.now() / 1000) - 90000 },
    ];
  let open = new Set();
  let mcp =
    JSON.parse(localStorage.getItem("ca_mcp") || "null") || {
      main: { Roblox_Studio: { command: "npx", args: ["-y", "roblox-studio-mcp"] } },
    };
  const save = () => localStorage.setItem("ca_mock", JSON.stringify(store));
  const saveMcp = () => localStorage.setItem("ca_mcp", JSON.stringify(mcp));
  return async (cmd, args = {}) => {
    switch (cmd) {
      case "get_status":
        return { installed: true, version: "1.18286.0 (preview)", mirror_ready: true };
      case "sync_mirror":
        return "1.18286.0 (preview)";
      case "get_mcp_servers":
        return mcp[args.id] || {};
      case "set_mcp_servers":
        mcp[args.id] = args.servers;
        saveMcp();
        return;
      case "list_accounts":
        return store.slice();
      case "add_account": {
        const a = { id: String(Date.now()), name: args.name.trim(), gradient: store.length % 6, created_at: 0, last_used: 0 };
        store.push(a);
        save();
        return a;
      }
      case "rename_account": {
        const a = store.find((x) => x.id === args.id);
        if (a) a.name = args.name.trim();
        save();
        return;
      }
      case "delete_account":
        store = store.filter((x) => x.id !== args.id);
        open.delete(args.id);
        save();
        return;
      case "open_account":
        open.add(args.id);
        return;
      case "close_account":
        open.delete(args.id);
        return;
      case "open_state":
        return [...open];
      default:
        return null;
    }
  };
}

const invoke = (window.__TAURI__ && window.__TAURI__.core.invoke) || makeMockInvoke();

const GRAD = 6;
let accounts = [];
let openIds = new Set();
let installed = false;

const grid = document.getElementById("grid");
const emptyEl = document.getElementById("empty");
const notInstalledEl = document.getElementById("notInstalled");
const menu = document.getElementById("menu");
const addBtn = document.getElementById("addBtn");
const statusChip = document.getElementById("claudeStatus");
const statusText = document.getElementById("statusText");
const prepping = document.getElementById("prepping");
const tipEl = document.getElementById("tip");
const tipCloseEl = document.getElementById("tipClose");
const syncOverlay = document.getElementById("syncOverlay");
const syncSourceSel = document.getElementById("syncSource");
const syncBody = document.getElementById("syncBody");
const syncEmpty = document.getElementById("syncEmpty");

/* ---------------- helpers ---------------- */
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}
function initials(name) {
  const p = name.trim().split(/\s+/).filter(Boolean);
  if (!p.length) return "?";
  if (p.length === 1) return p[0].slice(0, 2).toUpperCase();
  return (p[0][0] + p[p.length - 1][0]).toUpperCase();
}
function relTime(ts) {
  if (!ts) return "Never opened";
  const s = Math.floor(Date.now() / 1000) - ts;
  if (s < 60) return "Opened just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `Opened ${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `Opened ${h} hr ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return `Opened ${d} day${d > 1 ? "s" : ""} ago`;
  return "Opened a while ago";
}

/* ---------------- toast ---------------- */
function toast(msg) {
  const t = document.createElement("div");
  t.className = "toast";
  t.textContent = msg;
  document.getElementById("toasts").appendChild(t);
  requestAnimationFrame(() => t.classList.add("show"));
  setTimeout(() => {
    t.classList.remove("show");
    setTimeout(() => t.remove(), 320);
  }, 2800);
}

/* ---------------- status + views ---------------- */
function applyStatus(s) {
  installed = !!s.installed;
  statusChip.classList.remove("is-checking", "is-ok", "is-warn", "is-bad");
  if (!s.installed) {
    statusChip.classList.add("is-bad");
    statusText.textContent = "Claude not installed";
  } else if (!s.mirror_ready) {
    statusChip.classList.add("is-warn");
    statusText.textContent = `Claude ${s.version} · preparing`;
  } else {
    statusChip.classList.add("is-ok");
    statusText.textContent = `Claude ${s.version} · isolated`;
  }
}
function setView() {
  notInstalledEl.style.display = installed ? "none" : "flex";
  addBtn.disabled = !installed;
  const n = accounts.length;
  emptyEl.style.display = installed && n === 0 ? "flex" : "none";
  grid.style.display = installed && n > 0 ? "grid" : "none";
  if (tipEl) tipEl.hidden = !(installed && !localStorage.getItem("ca_tip_dismissed"));
}
function showPrepping(on) {
  prepping.hidden = !on;
}

/* ---------------- data ---------------- */
async function loadAccounts() {
  accounts = await invoke("list_accounts");
  render();
}
async function refreshOpen() {
  if (!installed) return;
  try {
    openIds = new Set(await invoke("open_state"));
    document
      .querySelectorAll(".card")
      .forEach((c) => c.classList.toggle("is-open", openIds.has(c.dataset.id)));
  } catch (e) {
    /* ignore */
  }
}

/* ---------------- render ---------------- */
function render() {
  setView();
  grid.innerHTML = "";
  accounts.forEach((a, i) => {
    const g = a.gradient % GRAD;
    const card = document.createElement("div");
    card.className = "card" + (openIds.has(a.id) ? " is-open" : "");
    card.dataset.id = a.id;
    card.style.animationDelay = `${Math.min(i, 12) * 45}ms`;
    card.innerHTML = `
      <div class="avatar g${g}">${escapeHtml(initials(a.name))}<span class="dot"></span></div>
      <div class="info">
        <div class="name">${escapeHtml(a.name)}</div>
        <div class="meta">
          <span class="status">&#9679; Running</span>
          <span class="last">${relTime(a.last_used)}</span>
        </div>
      </div>
      <button class="kebab" data-id="${a.id}" aria-label="More options">&#8943;</button>`;
    grid.appendChild(card);
  });
}

/* ---------------- account actions ---------------- */
async function openAccount(a) {
  if (!installed) return;
  const wasOpen = openIds.has(a.id);
  try {
    if (!wasOpen) toast(`Launching ${a.name}…`);
    await invoke("open_account", { id: a.id });
    if (wasOpen) toast(`${a.name} is already open`);
    a.last_used = Math.floor(Date.now() / 1000);
    setTimeout(refreshOpen, 600);
    setTimeout(render, 800);
  } catch (e) {
    toast(String(e));
  }
}
async function closeAccount(a) {
  try {
    await invoke("close_account", { id: a.id });
    toast(`Closed ${a.name}`);
    setTimeout(refreshOpen, 250);
  } catch (e) {
    /* ignore */
  }
}

/* ---------------- context menu ---------------- */
function openMenu(id, x, y) {
  const a = accounts.find((z) => z.id === id);
  if (!a) return;
  const isOpen = openIds.has(id);
  const items = [];
  if (isOpen) items.push({ label: "Close window", fn: () => closeAccount(a) });
  else items.push({ label: "Open", fn: () => openAccount(a) });
  items.push({ label: "Rename", fn: () => renameFlow(a) });
  items.push({ label: "Delete", danger: true, fn: () => deleteFlow(a) });

  menu.innerHTML = "";
  items.forEach((it) => {
    const b = document.createElement("button");
    b.className = "menu-item" + (it.danger ? " danger" : "");
    b.textContent = it.label;
    b.onclick = () => { hideMenu(); it.fn(); };
    menu.appendChild(b);
  });
  menu.hidden = false;
  const mw = menu.offsetWidth, mh = menu.offsetHeight;
  menu.style.left = Math.min(x, window.innerWidth - mw - 12) + "px";
  menu.style.top = Math.min(y, window.innerHeight - mh - 12) + "px";
}
function hideMenu() { menu.hidden = true; }

/* ---------------- modal ---------------- */
const overlay = document.getElementById("overlay");
const mTitle = document.getElementById("modalTitle");
const mBody = document.getElementById("modalBody");
const mInput = document.getElementById("modalInput");
const mConfirm = document.getElementById("modalConfirm");
const mCancel = document.getElementById("modalCancel");
let confirmHandler = null;

function showModal(opts) {
  const { title, body = "", value = "", placeholder = "", confirmLabel = "Confirm", danger = false, withInput = true, onConfirm } = opts;
  mTitle.textContent = title;
  mBody.textContent = body;
  mBody.style.display = body ? "block" : "none";
  mInput.style.display = withInput ? "block" : "none";
  mInput.value = value;
  mInput.placeholder = placeholder;
  mConfirm.textContent = confirmLabel;
  mConfirm.classList.toggle("btn-danger", danger);
  mConfirm.classList.toggle("btn-primary", !danger);
  confirmHandler = onConfirm;
  overlay.hidden = false;
  if (withInput) setTimeout(() => { mInput.focus(); mInput.select(); }, 30);
  else setTimeout(() => mConfirm.focus(), 30);
}
function hideModal() { overlay.hidden = true; confirmHandler = null; }

mConfirm.onclick = () => { if (confirmHandler) confirmHandler(mInput.value.trim()); };
mCancel.onclick = hideModal;
overlay.addEventListener("click", (e) => { if (e.target === overlay) hideModal(); });
mInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); mConfirm.click(); }
  else if (e.key === "Escape") hideModal();
});

/* ---------------- flows ---------------- */
function addFlow() {
  if (!installed) return;
  showModal({
    title: "Add account",
    placeholder: "e.g. Work, Personal, Client A",
    confirmLabel: "Add",
    onConfirm: async (v) => {
      if (!v) { mInput.focus(); return; }
      const acc = await invoke("add_account", { name: v });
      accounts.push(acc);
      hideModal();
      render();
      toast(`Added ${acc.name}`);
    },
  });
}
function renameFlow(a) {
  showModal({
    title: "Rename account",
    value: a.name,
    confirmLabel: "Save",
    onConfirm: async (v) => {
      if (!v) { mInput.focus(); return; }
      await invoke("rename_account", { id: a.id, name: v });
      a.name = v;
      hideModal();
      render();
    },
  });
}
function deleteFlow(a) {
  showModal({
    title: `Delete “${a.name}”?`,
    body: "This closes its window and permanently erases the saved login and data for this account on this machine.",
    withInput: false,
    danger: true,
    confirmLabel: "Delete",
    onConfirm: async () => {
      await invoke("delete_account", { id: a.id });
      accounts = accounts.filter((z) => z.id !== a.id);
      hideModal();
      render();
      toast(`Deleted ${a.name}`);
    },
  });
}

/* ---------------- events ---------------- */
grid.addEventListener("click", (e) => {
  const kebab = e.target.closest(".kebab");
  if (kebab) {
    e.stopPropagation();
    const r = kebab.getBoundingClientRect();
    openMenu(kebab.dataset.id, r.right - 4, r.bottom + 6);
    return;
  }
  const card = e.target.closest(".card");
  if (card) {
    const a = accounts.find((z) => z.id === card.dataset.id);
    if (a) openAccount(a);
  }
});
grid.addEventListener("mousemove", (e) => {
  const card = e.target.closest(".card");
  if (card) {
    const r = card.getBoundingClientRect();
    card.style.setProperty("--mx", `${e.clientX - r.left}px`);
  }
});
document.addEventListener("click", (e) => {
  if (!menu.hidden && !menu.contains(e.target) && !e.target.closest(".kebab")) hideMenu();
});
window.addEventListener("resize", hideMenu);
addBtn.onclick = addFlow;
document.getElementById("addBtn2").onclick = addFlow;
document.getElementById("recheckBtn").onclick = boot;
if (tipCloseEl) {
  tipCloseEl.onclick = () => {
    localStorage.setItem("ca_tip_dismissed", "1");
    if (tipEl) tipEl.hidden = true;
  };
}

/* ---------------- config sync (MCP servers) ---------------- */
let syncState = { source: "main", sourceServers: {}, targets: [] };

async function openSync() {
  if (!installed) {
    toast("Claude Desktop isn't installed");
    return;
  }
  syncSourceSel.innerHTML = "";
  const opts = [{ id: "main", name: "Main Claude account" }, ...accounts.map((a) => ({ id: a.id, name: a.name }))];
  opts.forEach((o) => {
    const el = document.createElement("option");
    el.value = o.id;
    el.textContent = o.name;
    syncSourceSel.appendChild(el);
  });
  syncOverlay.hidden = false;
  await loadSyncMatrix();
}

async function loadSyncMatrix() {
  const source = syncSourceSel.value || "main";
  const sourceServers = (await invoke("get_mcp_servers", { id: source })) || {};
  const names = Object.keys(sourceServers);
  syncState = { source, sourceServers, targets: [] };
  syncBody.innerHTML = "";

  const targets = accounts.filter((a) => a.id !== source);
  if (!names.length || !targets.length) {
    syncEmpty.hidden = false;
    syncEmpty.textContent = !names.length
      ? "No MCP servers found in the selected source."
      : "Add another account to sync into.";
    syncBody.style.display = "none";
    return;
  }
  syncEmpty.hidden = true;
  syncBody.style.display = "flex";

  for (const acc of targets) {
    const current = (await invoke("get_mcp_servers", { id: acc.id })) || {};
    const rows = names
      .map((n) => {
        const checked = Object.prototype.hasOwnProperty.call(current, n) ? "checked" : "";
        return `<label class="sync-server"><input type="checkbox" data-acc="${escapeHtml(acc.id)}" data-name="${escapeHtml(n)}" ${checked}/> <code>${escapeHtml(n)}</code></label>`;
      })
      .join("");
    const sec = document.createElement("div");
    sec.className = "sync-account";
    sec.innerHTML = `<div class="sync-account-name">${escapeHtml(acc.name)}</div>${rows}`;
    syncBody.appendChild(sec);
    syncState.targets.push({ id: acc.id, current });
  }
}

async function applySync() {
  const names = Object.keys(syncState.sourceServers);
  let count = 0;
  for (const t of syncState.targets) {
    const desired = {};
    // keep servers the target already has that aren't part of the source set
    for (const [k, v] of Object.entries(t.current)) {
      if (!names.includes(k)) desired[k] = v;
    }
    // add the ticked source servers
    syncBody.querySelectorAll(`input[data-acc="${cssEscape(t.id)}"]`).forEach((cb) => {
      if (cb.checked) desired[cb.dataset.name] = syncState.sourceServers[cb.dataset.name];
    });
    await invoke("set_mcp_servers", { id: t.id, servers: desired });
    count++;
  }
  syncOverlay.hidden = true;
  toast(count ? `Synced ${count} account${count > 1 ? "s" : ""}` : "Nothing to sync");
}

function cssEscape(s) {
  return String(s).replace(/["\\]/g, "\\$&");
}

document.getElementById("syncBtn").onclick = openSync;
document.getElementById("syncCancel").onclick = () => {
  syncOverlay.hidden = true;
};
document.getElementById("syncApply").onclick = applySync;
syncSourceSel.onchange = loadSyncMatrix;
syncOverlay.addEventListener("click", (e) => {
  if (e.target === syncOverlay) syncOverlay.hidden = true;
});

/* ---------------- boot ---------------- */
let pollTimer = null;
async function boot() {
  const status = await invoke("get_status");
  applyStatus(status);
  if (!status.installed) { setView(); return; }

  await loadAccounts();

  if (!status.mirror_ready) {
    showPrepping(true);
    try {
      const ver = await invoke("sync_mirror");
      applyStatus({ installed: true, version: ver, mirror_ready: true });
    } catch (e) {
      toast(String(e));
      applyStatus({ installed: true, version: status.version, mirror_ready: false });
    }
    showPrepping(false);
  }

  refreshOpen();
  if (!pollTimer) pollTimer = setInterval(refreshOpen, 1500);
}

boot();
