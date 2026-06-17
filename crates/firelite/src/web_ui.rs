use axum::response::Html;

pub async fn console() -> Html<&'static str> {
    Html(CONSOLE_HTML)
}

const CONSOLE_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Firelite Console</title>
<style>
:root {
  color-scheme: dark;
  --bg: #11130f;
  --panel: #191d17;
  --panel-2: #20261d;
  --line: #394031;
  --text: #f4f0df;
  --muted: #aaa38d;
  --accent: #f2b84b;
  --green: #78c28d;
  --red: #e36f5f;
  --blue: #82aaff;
  --shadow: 0 18px 70px rgb(0 0 0 / 36%);
}
* { box-sizing: border-box; }
body {
  margin: 0;
  min-height: 100vh;
  background:
    linear-gradient(120deg, rgb(242 184 75 / 10%), transparent 28rem),
    radial-gradient(circle at top right, rgb(120 194 141 / 12%), transparent 24rem),
    var(--bg);
  color: var(--text);
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  letter-spacing: 0;
}
button, input, textarea {
  font: inherit;
}
.shell {
  width: min(1480px, calc(100vw - 32px));
  margin: 0 auto;
  padding: 28px 0 36px;
}
.topbar {
  display: grid;
  grid-template-columns: minmax(260px, 1fr) auto;
  gap: 20px;
  align-items: end;
  margin-bottom: 22px;
}
.brand {
  display: flex;
  gap: 14px;
  align-items: center;
}
.mark {
  width: 42px;
  height: 42px;
  display: grid;
  place-items: center;
  background: var(--accent);
  color: #16130b;
  border: 1px solid #ffd27a;
  box-shadow: 6px 6px 0 #000;
  font-weight: 900;
}
h1 {
  margin: 0;
  font-size: clamp(30px, 4vw, 54px);
  line-height: 0.92;
  font-weight: 900;
}
.status {
  color: var(--muted);
  margin-top: 8px;
  min-height: 20px;
}
.controls {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 10px;
}
.field {
  display: grid;
  gap: 6px;
  color: var(--muted);
  font-size: 12px;
  text-transform: uppercase;
}
input, textarea {
  width: 100%;
  border: 1px solid var(--line);
  background: #0c0e0b;
  color: var(--text);
  min-height: 38px;
  padding: 9px 10px;
  outline: none;
  border-radius: 0;
}
textarea {
  min-height: 108px;
  resize: vertical;
}
input:focus, textarea:focus {
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgb(242 184 75 / 18%);
}
.controls input {
  width: 230px;
}
.tabs {
  display: inline-grid;
  grid-template-columns: 1fr 1fr;
  gap: 0;
  border: 1px solid var(--line);
  background: #0c0e0b;
  box-shadow: var(--shadow);
}
.tab {
  min-width: 148px;
  border: 0;
  border-right: 1px solid var(--line);
  background: transparent;
  color: var(--muted);
  padding: 12px 18px;
  cursor: pointer;
}
.tab:last-child { border-right: 0; }
.tab[aria-selected="true"] {
  background: var(--accent);
  color: #171309;
}
.grid {
  display: grid;
  grid-template-columns: 360px minmax(0, 1fr);
  gap: 18px;
  align-items: start;
}
.panel {
  border: 1px solid var(--line);
  background: rgb(25 29 23 / 92%);
  box-shadow: var(--shadow);
}
.panel-head {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: center;
  padding: 14px 16px;
  border-bottom: 1px solid var(--line);
  background: var(--panel-2);
}
h2, h3 {
  margin: 0;
  font-size: 15px;
  text-transform: uppercase;
}
.panel-body {
  padding: 16px;
}
.stack {
  display: grid;
  gap: 12px;
}
.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 9px;
}
button {
  min-height: 38px;
  border: 1px solid var(--line);
  background: #0c0e0b;
  color: var(--text);
  padding: 9px 12px;
  cursor: pointer;
  border-radius: 0;
}
button:hover {
  border-color: var(--accent);
  color: var(--accent);
}
.primary {
  background: var(--accent);
  border-color: var(--accent);
  color: #15120a;
  font-weight: 800;
}
.primary:hover {
  color: #15120a;
  filter: brightness(1.06);
}
.danger:hover {
  border-color: var(--red);
  color: var(--red);
}
.ghost {
  color: var(--muted);
}
.table-wrap {
  overflow: auto;
}
table {
  width: 100%;
  border-collapse: collapse;
  min-width: 760px;
}
th, td {
  border-bottom: 1px solid var(--line);
  padding: 12px 14px;
  text-align: left;
  vertical-align: top;
}
th {
  color: var(--muted);
  background: #12150f;
  font-size: 12px;
  text-transform: uppercase;
  position: sticky;
  top: 0;
  z-index: 1;
}
td {
  font-size: 13px;
}
.mono {
  word-break: break-all;
}
.pill {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  padding: 3px 8px;
  border: 1px solid var(--line);
  background: #0c0e0b;
  color: var(--muted);
  font-size: 12px;
}
.ok { color: var(--green); }
.bad { color: var(--red); }
.blue { color: var(--blue); }
.empty {
  padding: 50px 18px;
  color: var(--muted);
  text-align: center;
  border-top: 1px solid var(--line);
}
.view {
  display: none;
}
.view.active {
  display: block;
}
@media (max-width: 900px) {
  .topbar, .grid {
    grid-template-columns: 1fr;
  }
  .controls {
    justify-content: stretch;
  }
  .controls input, .tab {
    width: 100%;
  }
  .tabs {
    width: 100%;
  }
}
</style>
</head>
<body>
<main class="shell">
  <header class="topbar">
    <div class="brand">
      <div class="mark">FL</div>
      <div>
        <h1>Firelite Console</h1>
        <div class="status" id="status">Connecting to local daemon...</div>
      </div>
    </div>
    <div class="controls">
      <label class="field">Project
        <input id="project" value="demo-firelite" autocomplete="off">
      </label>
      <label class="field">Bucket
        <input id="bucket" value="demo-firelite.appspot.com" autocomplete="off">
      </label>
    </div>
  </header>

  <nav class="tabs" aria-label="Console views">
    <button class="tab" data-view="auth" aria-selected="true">Auth</button>
    <button class="tab" data-view="storage" aria-selected="false">Storage</button>
  </nav>

  <section id="auth" class="view active">
    <div class="grid" style="margin-top: 18px;">
      <aside class="panel">
        <div class="panel-head"><h2>Create User</h2></div>
        <form class="panel-body stack" id="create-user">
          <label class="field">Email
            <input name="email" type="email" required placeholder="alice@example.test">
          </label>
          <label class="field">Password
            <input name="password" type="password" minlength="6" required placeholder="secret123">
          </label>
          <label class="field">Display name
            <input name="displayName" placeholder="Alice">
          </label>
          <div class="actions">
            <button class="primary" type="submit">Create</button>
            <button type="button" id="refresh-auth">Refresh</button>
            <button class="danger" type="button" id="reset-auth">Reset</button>
          </div>
        </form>
        <div class="panel-head"><h2>OOB Codes</h2></div>
        <div class="panel-body stack">
          <div id="oob-list" class="stack"></div>
        </div>
      </aside>
      <section class="panel">
        <div class="panel-head">
          <h2>Users</h2>
          <span class="pill" id="user-count">0 users</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Email</th>
                <th>Local ID</th>
                <th>Providers</th>
                <th>Created</th>
                <th></th>
              </tr>
            </thead>
            <tbody id="users"></tbody>
          </table>
          <div id="users-empty" class="empty">No users in this project.</div>
        </div>
      </section>
    </div>
  </section>

  <section id="storage" class="view">
    <div class="grid" style="margin-top: 18px;">
      <aside class="panel">
        <div class="panel-head"><h2>Create Object</h2></div>
        <form class="panel-body stack" id="create-object">
          <label class="field">Object path
            <input name="name" required placeholder="uploads/sample.txt">
          </label>
          <label class="field">Content type
            <input name="contentType" value="text/plain">
          </label>
          <label class="field">Content
            <textarea name="content" placeholder="local test payload"></textarea>
          </label>
          <div class="actions">
            <button class="primary" type="submit">Create</button>
            <button type="button" id="refresh-storage">Refresh</button>
            <button class="danger" type="button" id="reset-storage">Reset</button>
          </div>
        </form>
      </aside>
      <section class="panel">
        <div class="panel-head">
          <h2>Objects</h2>
          <span class="pill" id="object-count">0 objects</span>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Type</th>
                <th>Size</th>
                <th>Updated</th>
                <th></th>
              </tr>
            </thead>
            <tbody id="objects"></tbody>
          </table>
          <div id="objects-empty" class="empty">No objects in this bucket.</div>
        </div>
      </section>
    </div>
  </section>
</main>

<script>
const state = {
  project: document.querySelector("#project").value,
  bucket: document.querySelector("#bucket").value,
  view: "auth"
};

const $ = (selector) => document.querySelector(selector);

function endpoint(path) {
  return path
    .replaceAll(":project", encodeURIComponent(state.project))
    .replaceAll(":bucket", encodeURIComponent(state.bucket));
}

function setStatus(message, kind = "") {
  const node = $("#status");
  node.textContent = message;
  node.className = "status " + kind;
}

async function api(path, options = {}) {
  const response = await fetch(endpoint(path), {
    headers: { "content-type": "application/json", ...(options.headers || {}) },
    ...options
  });
  if (!response.ok) {
    let message = response.statusText;
    try {
      const body = await response.json();
      message = body.error?.message || message;
    } catch (_) {}
    throw new Error(message);
  }
  if (response.status === 204) return {};
  return response.json();
}

function formatTime(ms) {
  const value = Number(ms);
  if (!Number.isFinite(value) || value <= 0) return "";
  return new Date(value).toLocaleString();
}

function authBase() {
  return "/identitytoolkit.googleapis.com/v1/projects/:project";
}

async function loadAuth() {
  const [accounts, oob] = await Promise.all([
    api("/emulator/v1/projects/:project/accounts"),
    api("/emulator/v1/projects/:project/oobCodes")
  ]);
  renderUsers(accounts.users || []);
  renderOob(oob.oobCodes || []);
}

function renderUsers(users) {
  $("#user-count").textContent = `${users.length} ${users.length === 1 ? "user" : "users"}`;
  $("#users-empty").style.display = users.length ? "none" : "block";
  $("#users").innerHTML = users.map((user) => {
    const providers = (user.providerUserInfo || []).map((provider) => provider.providerId).join(", ");
    return `<tr>
      <td>
        <div>${escapeHtml(user.email || "")}</div>
        <span class="pill ${user.emailVerified ? "ok" : ""}">${user.emailVerified ? "verified" : "unverified"}</span>
        ${user.disabled ? `<span class="pill bad">disabled</span>` : ""}
      </td>
      <td class="mono">${escapeHtml(user.localId || "")}</td>
      <td>${escapeHtml(providers || "password")}</td>
      <td>${formatTime(user.createdAt)}</td>
      <td><button class="danger" data-delete-user="${escapeAttr(user.localId)}">Delete</button></td>
    </tr>`;
  }).join("");
}

function renderOob(codes) {
  $("#oob-list").innerHTML = codes.length ? codes.map((code) => `
    <div class="pill mono">${escapeHtml(code.email)} ${escapeHtml(code.requestType)} ${escapeHtml(code.oobCode)}</div>
  `).join("") : `<div class="pill">No OOB codes</div>`;
}

async function createUser(form) {
  const data = new FormData(form);
  await api(`${authBase()}/accounts`, {
    method: "POST",
    body: JSON.stringify({
      email: data.get("email"),
      password: data.get("password"),
      displayName: data.get("displayName") || undefined
    })
  });
  form.reset();
  await loadAuth();
}

async function loadStorage() {
  const body = await api("/v0/b/:bucket/o");
  renderObjects(body.items || []);
}

function renderObjects(objects) {
  $("#object-count").textContent = `${objects.length} ${objects.length === 1 ? "object" : "objects"}`;
  $("#objects-empty").style.display = objects.length ? "none" : "block";
  $("#objects").innerHTML = objects.map((object) => `
    <tr>
      <td class="mono">${escapeHtml(object.name)}</td>
      <td>${escapeHtml(object.contentType)}</td>
      <td>${object.size} B</td>
      <td>${formatTime(object.updated)}</td>
      <td><button class="danger" data-delete-object="${escapeAttr(object.name)}">Delete</button></td>
    </tr>
  `).join("");
}

async function createObject(form) {
  const data = new FormData(form);
  await api(`/v0/b/:bucket/o?name=${encodeURIComponent(data.get("name"))}`, {
    method: "POST",
    headers: { "content-type": data.get("contentType") || "application/octet-stream" },
    body: data.get("content") || ""
  });
  form.reset();
  form.elements.contentType.value = "text/plain";
  await loadStorage();
}

async function refreshActive() {
  state.project = $("#project").value.trim() || "demo-firelite";
  state.bucket = $("#bucket").value.trim() || "demo-firelite.appspot.com";
  try {
    if (state.view === "auth") await loadAuth();
    if (state.view === "storage") await loadStorage();
    setStatus(`Project ${state.project}`, "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (char) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;"
  })[char]);
}

function escapeAttr(value) {
  return escapeHtml(value).replace(/`/g, "&#96;");
}

document.querySelectorAll(".tab").forEach((button) => {
  button.addEventListener("click", async () => {
    state.view = button.dataset.view;
    document.querySelectorAll(".tab").forEach((tab) => {
      tab.setAttribute("aria-selected", String(tab === button));
    });
    document.querySelectorAll(".view").forEach((view) => {
      view.classList.toggle("active", view.id === state.view);
    });
    await refreshActive();
  });
});

$("#project").addEventListener("change", refreshActive);
$("#bucket").addEventListener("change", refreshActive);
$("#refresh-auth").addEventListener("click", refreshActive);
$("#refresh-storage").addEventListener("click", refreshActive);

$("#create-user").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await createUser(event.currentTarget);
    setStatus("User created", "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
});

$("#create-object").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await createObject(event.currentTarget);
    setStatus("Object created", "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
});

$("#users").addEventListener("click", async (event) => {
  const localId = event.target.dataset.deleteUser;
  if (!localId) return;
  try {
    await api(`${authBase()}/accounts:delete`, {
      method: "POST",
      body: JSON.stringify({ localId })
    });
    await loadAuth();
    setStatus("User deleted", "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
});

$("#objects").addEventListener("click", async (event) => {
  const objectName = event.target.dataset.deleteObject;
  if (!objectName) return;
  try {
    await api(`/v0/b/:bucket/o/${encodeURIComponent(objectName)}`, {
      method: "DELETE"
    });
    await loadStorage();
    setStatus("Object deleted", "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
});

$("#reset-auth").addEventListener("click", async () => {
  try {
    await api("/emulator/v1/projects/:project/accounts", { method: "DELETE" });
    await loadAuth();
    setStatus("Auth reset", "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
});

$("#reset-storage").addEventListener("click", async () => {
  try {
    await api("/emulator/v1/projects/:project/storage/buckets/:bucket/objects", { method: "DELETE" });
    await loadStorage();
    setStatus("Bucket reset", "ok");
  } catch (error) {
    setStatus(error.message, "bad");
  }
});

refreshActive();
</script>
</body>
</html>
"##;
