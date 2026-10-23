const assert = require("node:assert/strict");
const { spawn } = require("node:child_process");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const readline = require("node:readline");
const { test } = require("node:test");

const workerPath = path.resolve(__dirname, "../src/functions_worker.cjs");
const handler = `export const handler = Object.assign((req, res) => res.end("real handler"), {
  __trigger: { name: "api", regions: ["us-central1"], httpsTrigger: {} }
});`;

async function start(t, files, filters = ["api"], selectiveLoad = true) {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "firelite-selective-"));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  for (const [name, contents] of Object.entries(files)) {
    await fs.writeFile(path.join(dir, name), contents);
  }
  const child = spawn(process.execPath, [workerPath, dir, "demo-selective",
    JSON.stringify({ filters, selectiveLoad })], { stdio: ["ignore", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) {
      const exited = new Promise((resolve) => child.once("exit", resolve));
      child.kill();
      await exited;
    }
  });
  const lines = readline.createInterface({ input: child.stdout });
  const timer = setTimeout(() => child.kill(), 5000);
  try {
    for await (const line of lines) return JSON.parse(line);
    throw new Error(`worker exited without protocol message: ${stderr}`);
  } finally {
    clearTimeout(timer);
    lines.close();
  }
}

test("selective loading skips scheduled imports and invokes the real handler", async (t) => {
  const files = {
    "package.json": JSON.stringify({ type: "module" }),
    "index.js": `import './setup.js';
      import { handler } from './api.js';
      import { scheduled } from './scheduled.js';
      export const api = handler;
      export const cron = { scheduled };`,
    "setup.js": `globalThis.fireliteSetup = true;`,
    "api.js": `if (!globalThis.fireliteSetup) throw new Error("setup missing");\n${handler}`,
    "scheduled.js": `throw new Error("scheduled module loaded"); export const scheduled = {};`,
  };
  const normal = await start(t, files, ["api"], false);
  assert.equal(normal.type, "error");
  assert.match(normal.message, /scheduled module loaded/);
  const selected = await start(t, files);
  assert.equal(selected.type, "ready", selected.message);
  assert.deepEqual(selected.functions.map((fn) => fn.entryId), ["api"]);
  const response = await fetch(`http://127.0.0.1:${selected.port}/__firelite__/invoke/api`);
  assert.equal(await response.text(), "real handler");
});

test("tracks shared declarations, aliases, and individual import/declaration bindings", async (t) => {
  const selected = await start(t, {
    "package.json": '{"type":"module"}',
    "index.js": `import unusedDefault, { handler as real, unused } from './api.js'
      import * as discarded from './scheduled.js'
      const wrap = (fn) => fn;
      export const shared = real;
      var bad = discarded.explode(), selected = wrap(shared);
      var selected;
      export { selected as api, unused as cron };`,
    "api.js": handler + '\nexport default null; export const unused = null;',
    "scheduled.js": 'throw new Error("discarded dependency loaded");',
  });
  assert.equal(selected.type, "ready", selected.message);
  assert.deepEqual(selected.functions.map((fn) => fn.entryId), ["api"]);
  const response = await fetch(`http://127.0.0.1:${selected.port}/__firelite__/invoke/api`);
  assert.equal(await response.text(), "real handler");
});

test("named and namespace re-exports support repeated and nested filters", async (t) => {
  const selected = await start(t, {
    "package.json": '{"type":"module"}',
    "index.js": `export { handler as api } from './api.js'
      export * as tasks from './api.js';
      export { missing as cron } from './does-not-exist.js';`,
    "api.js": handler,
  }, ["api", "tasks.handler"]);
  assert.equal(selected.type, "ready", selected.message);
  assert.deepEqual(selected.functions.map((fn) => fn.entryId).sort(), ["api", "tasks.handler"]);
});

test("keeps setup statements, destructured dependencies, and the original module URL", async (t) => {
  const selected = await start(t, {
    "package.json": '{"type":"module"}',
    "index.js": `import { handler } from './api.js';
      const { handler: selected } = { handler };
      const setup = () => { globalThis.marker = import.meta.url; };
      await setup();
      export const api = selected;
      export const cron = (() => { throw new Error("discarded initializer"); })();`,
    "api.js": `export const handler = Object.assign((req, res) => res.end(globalThis.marker), {
      __trigger: { name: 'api', httpsTrigger: {} }
    });`,
  });
  assert.equal(selected.type, "ready", selected.message);
  const response = await fetch(`http://127.0.0.1:${selected.port}/__firelite__/invoke/api`);
  assert.match(await response.text(), /^file:\/\/.*\/index\.js$/);
});

test("supports default exports and namespace imports in .mjs entrypoints", async (t) => {
  const selected = await start(t, {
    "package.json": '{"main":"entry.mjs"}',
    "entry.mjs": `import * as api from './api.mjs'; export default api.handler;
      export const cron = (() => { throw new Error("unused"); })();`,
    "api.mjs": handler,
  }, ["default"]);
  assert.equal(selected.type, "ready", selected.message);
  assert.deepEqual(selected.functions.map((fn) => fn.entryId), ["default"]);
});

test("unsupported selections fail before evaluating the entrypoint", async (t) => {
  const cases = [
    ["CommonJS", { "index.js": 'throw new Error("evaluated");' }, ["api"], /ESM entrypoint/],
    ["wildcard exports", { "index.js": "export * from './missing.js';" }, ["api"], /export \*/],
    ["unknown export", { "index.js": "throw new Error('evaluated'); export const api = 1;" }, ["missing"], /cannot find export/],
    ["empty filters", { "index.js": "export const api = 1;" }, [], /requires at least one/],
    ["eval", { "index.js": "const value = 1; export const api = eval('value');" }, ["api"], /does not support eval/],
  ];
  for (const [name, files, filters, expected] of cases) {
    await t.test(name, async (t) => {
      const result = await start(t, {
        ...(name === "CommonJS" ? {} : { "package.json": '{"type":"module"}' }), ...files,
      }, filters);
      assert.equal(result.type, "error");
      assert.match(result.message, expected);
    });
  }
});
