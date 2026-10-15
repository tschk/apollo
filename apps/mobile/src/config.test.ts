import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import {
  instanceId,
  masked,
  parseDesktopState,
  providerName,
  resolveScope,
  rosterOrder,
  toDesktopJson,
  validateFolder,
  validateKey,
  type Instance,
} from "./config.ts";
import { planRoute, parseCompletion, scrub } from "./chat.ts";
import { providerById } from "./providers.ts";

const here = dirname(fileURLToPath(import.meta.url));

test("instance ids match the desktop slug rules", () => {
  assert.equal(instanceId("Research Lab", []), "research-lab");
  assert.equal(instanceId("Research Lab", ["research-lab"]), "research-lab-2");
  assert.equal(instanceId("Research Lab", ["research-lab", "research-lab-2"]), "research-lab-3");
  assert.equal(instanceId("   ", []), "apollo");
  assert.equal(instanceId("---", []), "apollo");
  assert.equal(instanceId("A--B", []), "a-b");
});

test("everywhere paths use ~/.apollo/instances like the desktop", () => {
  assert.deepEqual(
    resolveScope({ everywhere: true, folder: "", instanceId: "research", home: "/home/max" }),
    {
      workspace: "/home/max",
      config_dir: "/home/max/.apollo/instances/research",
      everywhere: true,
    },
  );
  assert.equal(
    resolveScope({ everywhere: true, folder: "/ignored", instanceId: "research" }).config_dir,
    "~/.apollo/instances/research",
  );
  assert.deepEqual(resolveScope({ everywhere: false, folder: "  ~/src/apollo  ", instanceId: "apollo" }), {
    workspace: "~/src/apollo",
    config_dir: "~/src/apollo",
    everywhere: false,
  });
});

test("custom provider names are slugs and do not collide", () => {
  const custom = providerById("custom");
  assert.ok(custom);
  assert.equal(providerName(custom, "", []), "custom");
  assert.equal(providerName(custom, "Lab Box", []), "custom-lab-box");
  assert.equal(providerName(custom, "Lab Box", ["custom-lab-box"]), "custom-lab-box-2");
});

test("pinned instances sort first without reshuffling the rest", () => {
  const row = (id: string, pinned: boolean): Instance => ({
    id,
    name: id,
    everywhere: false,
    workspace: "/w",
    config_dir: "/w",
    provider: "openai",
    model: "m",
    permission_profile: "auto",
    color: null,
    pinned,
  });
  const ordered = rosterOrder([row("a", false), row("b", true), row("c", false), row("d", true)]);
  assert.deepEqual(
    ordered.map((item) => item.id),
    ["b", "d", "a", "c"],
  );
});

test("a pre-instances desktop.json migrates, and secrets are dropped", () => {
  const secret = "sk-test-secret-value";
  const parsed = parseDesktopState({
    onboarded: true,
    workspace: "/home/max/src",
    provider: "openai",
    model: "gpt-5.4",
    permission_profile: "prompt",
    api_key: secret,
    instances: [],
  });
  assert.equal(parsed.instances.length, 1);
  assert.equal(parsed.instances[0].id, "apollo");
  assert.equal(parsed.instances[0].config_dir, "/home/max/src");
  assert.equal(parsed.active, "apollo");
  const json = toDesktopJson({
    ...parsed,
    instances: parsed.instances.map((item) => ({ ...item, api_key: secret }) as Instance),
  });
  assert.equal(json.includes(secret), false);
  assert.equal(json.includes("api_key"), false);
  assert.match(json, /"mode": "simple"/);
});

test("folder paths reject urls", () => {
  assert.equal(validateFolder(""), "a folder needs a path");
  assert.equal(validateFolder("https://example.com"), "a workspace is a directory, not a url");
  assert.equal(validateFolder("~/src/apollo"), null);
});

test("keys are checked and masked, never echoed by length past 24", () => {
  assert.equal(validateKey("api-key", "  "), "this provider needs a key");
  assert.equal(validateKey("api-key", "has space"), "that key contains characters an api key never has");
  assert.equal(validateKey("oauth", ""), null);
  assert.equal(validateKey("custom", ""), null);
  assert.equal(masked(""), "");
  assert.equal(masked("abcdef"), "••••••");
  assert.equal([...masked("x".repeat(40))].length, 24);
});

test("oauth and missing keys stay offline; ollama stays on this device", () => {
  assert.equal(planRoute({ auth: "oauth" }).kind, "offline");
  assert.match(planRoute({ auth: "oauth" }).label, /not on this phone/);
  assert.equal(planRoute({ auth: "api-key", baseUrl: "https://api.openai.com/v1", key: "" }).kind, "offline");
  const local = planRoute({ auth: "local", baseUrl: "http://127.0.0.1:11434/v1" });
  assert.equal(local.kind, "provider");
  assert.equal(local.url, "http://127.0.0.1:11434/v1/chat/completions");
  assert.match(local.label, /ollama/);
  const custom = planRoute({
    auth: "custom",
    baseUrl: "https://models.example/v1/chat/completions",
    key: "secret-key-value",
  });
  assert.equal(custom.url, "https://models.example/v1/chat/completions");
});

test("completion parsing and scrubbing", () => {
  assert.equal(
    parseCompletion({ choices: [{ message: { content: " hello " } }] }),
    "hello",
  );
  assert.equal(parseCompletion({ choices: [] }), null);
  assert.equal(scrub("token sk-live-123456 rejected", "sk-live-123456"), "token *** rejected");
  assert.equal(scrub("short", "ab"), "short");
});

test("app copy does not advertise controlling other computers", () => {
  const banned = [
    "multiple computers",
    "multi-machine",
    "remote control",
    "remote-control",
    "other computers",
    "control across",
  ];
  const files: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      if (name === "node_modules" || name === ".expo" || name.endsWith(".test.ts")) continue;
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else if (/\.(ts|tsx|md|json)$/.test(name) && name !== "package-lock.json") files.push(path);
    }
  };
  walk(join(here, ".."));
  const hits: string[] = [];
  for (const path of files) {
    const text = readFileSync(path, "utf8").toLowerCase();
    for (const phrase of banned) {
      if (text.includes(phrase)) hits.push(`${path}: ${phrase}`);
    }
  }
  assert.deepEqual(hits, []);
});
