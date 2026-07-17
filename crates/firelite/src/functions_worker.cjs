#!/usr/bin/env node

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const { pathToFileURL } = require("node:url");

const sourceDir = process.argv[2];
const projectId = process.argv[3] || "demo-firelite";
const writeProtocol = process.stdout.write.bind(process.stdout);
const writeUserStdout = process.stderr.write.bind(process.stderr);

process.stdout.write = writeUserStdout;

if (!sourceDir) {
  fail("missing functions source directory");
}

process.env.GCLOUD_PROJECT ||= projectId;
process.env.GOOGLE_CLOUD_PROJECT ||= projectId;
process.env.FUNCTIONS_EMULATOR ||= "true";
process.env.FIREBASE_CONFIG ||= JSON.stringify({ projectId });

main().catch((error) => fail(error && error.stack ? error.stack : String(error)));

async function main() {
  const entrypoint = resolveEntrypoint(sourceDir);
  const loaded = await loadEntrypoint(entrypoint, sourceDir);
  const functions = discoverFunctions(loaded);
  const handlers = new Map();

  for (const descriptor of functions) {
    if (descriptor.trigger.type === "https" || descriptor.trigger.type === "event") {
      const handler = getExport(loaded, descriptor.entryId);
      if (typeof handler === "function") {
        handlers.set(descriptor.entryId, { handler, descriptor });
      }
    }
  }

  const server = http.createServer((req, res) => {
    const parsed = new URL(req.url, "http://127.0.0.1");
    const match = parsed.pathname.match(/^\/__firelite__\/invoke\/([^/]+)(\/.*)?$/);
    if (!match) {
      res.statusCode = 404;
      res.end("not found");
      return;
    }

    const entryId = decodeURIComponent(match[1]);
    const registration = handlers.get(entryId);
    if (!registration) {
      res.statusCode = 404;
      res.end("function not found");
      return;
    }
    const { handler, descriptor } = registration;

    const suffix = match[2] || "/";
    req.url = `${suffix}${parsed.search}`;
    req.originalUrl = req.url;
    req.route ||= { path: suffix };

    invokeHandler(req, res, handler, descriptor).catch((error) => {
      if (!res.headersSent) {
        res.statusCode = 500;
      }
      res.end(error && error.stack ? error.stack : String(error));
    });
  });

  server.on("error", (error) => fail(error && error.stack ? error.stack : String(error)));
  server.listen(0, "127.0.0.1", () => {
    const address = server.address();
    write({
      type: "ready",
      port: address.port,
      functions,
    });
  });
}

async function invokeHandler(req, res, handler, descriptor) {
  if (descriptor.trigger.type === "event") {
    const event = await readJsonBody(req);
    const result = descriptor.generation === "gen1"
      ? handler(event.data, {
          eventId: event.id,
          timestamp: event.time,
          eventType: descriptor.trigger.eventType,
          resource: {
            service: "storage.googleapis.com",
            name: `projects/_/buckets/${event.data.bucket}/objects/${event.data.name}`,
            type: "storage#object",
          },
          params: {},
        })
      : handler(event);
    if (result && typeof result.then === "function") {
      await result;
    }
    res.statusCode = 204;
    res.end();
    return;
  }

  installHttpCompatibilityHelpers(req, res);
  if (descriptor.trigger.taskQueue) {
    req.body = await readJsonBody(req);
  }

  try {
    const result = handler(req, res);
    if (result && typeof result.then === "function") {
      await result;
    }
  } catch (error) {
    if (!res.headersSent) {
      res.statusCode = 500;
    }
    res.end(error && error.stack ? error.stack : String(error));
  }
}

function installHttpCompatibilityHelpers(req, res) {
  req.header ||= (name) => {
    const value = req.headers[String(name).toLowerCase()];
    return Array.isArray(value) ? value.join(", ") : value;
  };
  req.get ||= req.header;

  res.status ||= (code) => {
    res.statusCode = code;
    return res;
  };
  res.send ||= (body) => {
    if (body === undefined || body === null) {
      res.end();
      return res;
    }
    if (Buffer.isBuffer(body) || typeof body === "string") {
      res.end(body);
      return res;
    }
    if (!res.getHeader("content-type")) {
      res.setHeader("content-type", "application/json; charset=utf-8");
    }
    res.end(JSON.stringify(body));
    return res;
  };
  res.json ||= (body) => {
    if (!res.getHeader("content-type")) {
      res.setHeader("content-type", "application/json; charset=utf-8");
    }
    res.end(JSON.stringify(body));
    return res;
  };
}

function readJsonBody(req) {
  return new Promise((resolve, reject) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("error", reject);
    req.on("end", () => {
      if (!body) {
        resolve({});
        return;
      }
      try {
        resolve(JSON.parse(body));
      } catch (error) {
        reject(error);
      }
    });
  });
}

async function loadEntrypoint(entrypoint, source) {
  if (isEsmEntrypoint(entrypoint, source)) {
    return import(pathToFileURL(entrypoint).href);
  }

  return require(entrypoint);
}

function isEsmEntrypoint(entrypoint, source) {
  const extension = path.extname(entrypoint);
  if (extension === ".mjs") {
    return true;
  }
  if (extension === ".cjs") {
    return false;
  }

  const packageJsonPath = path.join(source, "package.json");
  if (!fs.existsSync(packageJsonPath)) {
    return false;
  }

  const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
  return packageJson.type === "module";
}

function resolveEntrypoint(source) {
  const packageJsonPath = path.join(source, "package.json");
  if (fs.existsSync(packageJsonPath)) {
    const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
    if (packageJson.main) {
      return path.resolve(source, packageJson.main);
    }
  }

  for (const candidate of ["index.js", "index.cjs"]) {
    const candidatePath = path.join(source, candidate);
    if (fs.existsSync(candidatePath)) {
      return candidatePath;
    }
  }

  throw new Error(`could not find functions entrypoint under ${source}`);
}

function discoverFunctions(rootExports) {
  const found = [];
  const seen = new WeakSet();

  walk(rootExports, [], (entryId, value) => {
    const descriptor = describeFunction(entryId, value);
    if (descriptor) {
      found.push(descriptor);
    }
  }, seen);

  return found;
}

function walk(value, pathParts, visit, seen) {
  if (typeof value === "function") {
    visit(pathParts.join("."), value);
    return;
  }

  if (!value || typeof value !== "object") {
    return;
  }
  if (seen.has(value)) {
    return;
  }
  seen.add(value);

  for (const [key, child] of Object.entries(value)) {
    walk(child, pathParts.concat(key), visit, seen);
  }
}

function describeFunction(entryId, value) {
  if (typeof value !== "function") {
    return null;
  }

  if (value.__endpoint && value.__endpoint.platform === "gcfv2") {
    return describeGen2(entryId, value.__endpoint);
  }

  if (value.__trigger) {
    return describeGen1(entryId, value.__trigger, value);
  }

  if (value.__endpoint) {
    return describeGen2(entryId, value.__endpoint);
  }

  return null;
}

function describeGen1(entryId, trigger, value) {
  const name = trigger.name || entryId;
  const regions = trigger.regions || [trigger.region || "us-central1"];
  const schedule = value.__schedule || trigger.scheduleTrigger;

  if (schedule) {
    return {
      entryId,
      name,
      region: regions[0],
      generation: "gen1",
      trigger: {
        type: "schedule",
        schedule: schedule.schedule || schedule,
        timeZone: schedule.timeZone || schedule.time_zone || null,
        retryConfig: schedule.retryConfig || schedule.retry_config || null,
        topic: trigger.eventTrigger ? trigger.eventTrigger.resource || null : null,
      },
    };
  }

  if (trigger.httpsTrigger) {
    return {
      entryId,
      name,
      region: regions[0],
      generation: "gen1",
      trigger: {
        type: "https",
        callable: Boolean(
          trigger.labels &&
            (trigger.labels["deployment-callable"] === "true" ||
              trigger.labels["deployment-callable"] === true),
        ),
      },
    };
  }

  if (trigger.taskQueueTrigger) {
    return {
      entryId,
      name,
      region: regions[0],
      generation: "gen1",
      trigger: {
        type: "https",
        callable: false,
        taskQueue: true,
      },
    };
  }

  if (trigger.eventTrigger) {
    const resource = trigger.eventTrigger.resource || null;
    if (
      value.__schedule ||
      (trigger.eventTrigger.eventType === "google.pubsub.topic.publish" &&
        typeof resource === "string" &&
        resource.includes("/topics/firebase-schedule-"))
    ) {
      return {
        entryId,
        name,
        region: regions[0],
        generation: "gen1",
        trigger: {
          type: "schedule",
          schedule: value.__schedule ? value.__schedule.schedule || value.__schedule : null,
          timeZone: value.__schedule ? value.__schedule.timeZone || value.__schedule.time_zone || null : null,
          retryConfig: value.__schedule ? value.__schedule.retryConfig || value.__schedule.retry_config || null : null,
          topic: resource,
        },
      };
    }

    return {
      entryId,
      name,
      region: regions[0],
      generation: "gen1",
      trigger: {
        type: "event",
        eventType: trigger.eventTrigger.eventType || null,
        resource: trigger.eventTrigger.resource || null,
      },
    };
  }

  return null;
}

function describeGen2(entryId, endpoint) {
  const name = endpoint.id || endpoint.name || entryId.replace(/\./g, "-");
  const region = Array.isArray(endpoint.region) ? endpoint.region[0] : endpoint.region || "us-central1";

  if (endpoint.scheduleTrigger) {
    return {
      entryId,
      name,
      region,
      generation: "gen2",
      trigger: {
        type: "schedule",
        schedule: endpoint.scheduleTrigger.schedule || endpoint.scheduleTrigger,
        timeZone: endpoint.scheduleTrigger.timeZone || endpoint.scheduleTrigger.time_zone || null,
        retryConfig: endpoint.scheduleTrigger.retryConfig || endpoint.scheduleTrigger.retry_config || null,
      },
    };
  }

  if (endpoint.httpsTrigger || endpoint.callableTrigger) {
    return {
      entryId,
      name,
      region,
      generation: "gen2",
      trigger: {
        type: "https",
        callable: Boolean(endpoint.callableTrigger),
      },
    };
  }

  if (endpoint.taskQueueTrigger) {
    return {
      entryId,
      name,
      region,
      generation: "gen2",
      trigger: {
        type: "https",
        callable: false,
        taskQueue: true,
      },
    };
  }

  if (endpoint.eventTrigger) {
    return {
      entryId,
      name,
      region,
      generation: "gen2",
      trigger: {
        type: "event",
        eventType: endpoint.eventTrigger.eventType || null,
        resource: endpoint.eventTrigger.eventFilters || null,
      },
    };
  }

  return null;
}

function getExport(rootExports, entryId) {
  return entryId.split(".").reduce((value, part) => value && value[part], rootExports);
}

function write(payload) {
  writeProtocol(`${JSON.stringify(payload)}\n`);
}

function fail(message) {
  write({ type: "error", message });
  process.exit(1);
}
