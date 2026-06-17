#!/usr/bin/env node

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const sourceDir = process.argv[2];
const projectId = process.argv[3] || "demo-firelite";

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
  const loaded = require(entrypoint);
  const functions = discoverFunctions(loaded);
  const handlers = new Map();

  for (const descriptor of functions) {
    if (descriptor.trigger.type === "https") {
      const handler = getExport(loaded, descriptor.entryId);
      if (typeof handler === "function") {
        handlers.set(descriptor.entryId, handler);
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
    const handler = handlers.get(entryId);
    if (!handler) {
      res.statusCode = 404;
      res.end("function not found");
      return;
    }

    req.url = `${match[2] || "/"}${parsed.search}`;
    try {
      const result = handler(req, res);
      if (result && typeof result.then === "function") {
        result.catch((error) => {
          if (!res.headersSent) {
            res.statusCode = 500;
          }
          res.end(error && error.stack ? error.stack : String(error));
        });
      }
    } catch (error) {
      if (!res.headersSent) {
        res.statusCode = 500;
      }
      res.end(error && error.stack ? error.stack : String(error));
    }
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

  walk(rootExports, [], (entryId, value) => {
    const descriptor = describeFunction(entryId, value);
    if (descriptor) {
      found.push(descriptor);
    }
  });

  return found;
}

function walk(value, pathParts, visit) {
  if (typeof value === "function") {
    visit(pathParts.join("."), value);
    return;
  }

  if (!value || typeof value !== "object") {
    return;
  }

  for (const [key, child] of Object.entries(value)) {
    walk(child, pathParts.concat(key), visit);
  }
}

function describeFunction(entryId, value) {
  if (typeof value !== "function") {
    return null;
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
  const name = endpoint.id || endpoint.name || entryId;
  const region = Array.isArray(endpoint.region) ? endpoint.region[0] : endpoint.region || "us-central1";

  if (endpoint.scheduleTrigger) {
    return {
      entryId,
      name,
      region,
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
      trigger: {
        type: "https",
        callable: Boolean(endpoint.callableTrigger),
      },
    };
  }

  if (endpoint.eventTrigger) {
    return {
      entryId,
      name,
      region,
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
  process.stdout.write(`${JSON.stringify(payload)}\n`);
}

function fail(message) {
  write({ type: "error", message });
  process.exit(1);
}
