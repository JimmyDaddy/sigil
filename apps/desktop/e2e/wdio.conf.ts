import {
  mkdirSync,
  mkdtempSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";

import { startDesktopProviderFixture } from "./provider-fixture";

const DESKTOP_E2E_IDENTIFIER = "dev.sigil.desktop.e2e";
const providerFixture = await startDesktopProviderFixture();
process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL = providerFixture.baseUrl;
const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT
  ?? mkdtempSync(join(tmpdir(), "sigil-desktop-e2e-"));
process.env.SIGIL_DESKTOP_E2E_ROOT = runtimeRoot;

const e2eHome = join(runtimeRoot, "home");
const workspaceRoot = join(runtimeRoot, "workspace");
const stateHome = join(runtimeRoot, "state");
const cacheHome = join(runtimeRoot, "cache");
const artifactRoot = resolve("..", "..", "target", "desktop-e2e-artifacts");
const applicationConfigRoot = process.platform === "darwin"
  ? join(e2eHome, "Library", "Application Support", DESKTOP_E2E_IDENTIFIER)
  : process.platform === "win32"
    ? join(e2eHome, "AppData", "Roaming", DESKTOP_E2E_IDENTIFIER)
    : join(e2eHome, ".config", DESKTOP_E2E_IDENTIFIER);

for (const directory of [
  join(e2eHome, ".sigil"),
  workspaceRoot,
  join(workspaceRoot, ".sigil", "skills", "desktop-e2e-skill"),
  join(workspaceRoot, ".sigil", "agents", "desktop-e2e-agent"),
  stateHome,
  cacheHome,
  artifactRoot,
  applicationConfigRoot,
]) {
  mkdirSync(directory, { recursive: true });
}

writeFileSync(
  join(e2eHome, ".sigil", "sigil.toml"),
  `config_version = 2

[workspace]
root = "."

[agent]
connection = "desktop-e2e"
model = "sigil-e2e-model"

[task]
enabled = true
routing_policy = "auto"
multi_agent_mode = "proactive"
max_subagents = 4
max_parallel_read_steps = 2

[connections.desktop-e2e]
label = "Desktop E2E"
provider = "custom"
protocol = "chat_completions"
base_url = "${providerFixture.baseUrl}"
credential = { source = "none" }
`,
  { encoding: "utf8", mode: 0o600 },
);
writeFileSync(
  join(applicationConfigRoot, "recent-workspaces-v1.json"),
  `${JSON.stringify({
    schemaVersion: 1,
    entries: [{
      id: "desktop-e2e-workspace",
      displayName: "desktop-e2e-workspace",
      workspaceRoot,
    }],
  }, null, 2)}\n`,
  "utf8",
);
writeFileSync(
  join(workspaceRoot, "README.md"),
  "# Sigil Desktop E2E workspace\n",
  "utf8",
);
writeFileSync(
  join(workspaceRoot, "desktop-e2e-large-output.txt"),
  `DESKTOP_E2E_ARTIFACT_PAGE_ONE\n${"a".repeat(17_000)}\nDESKTOP_E2E_ARTIFACT_PAGE_TWO\n${"b".repeat(17_000)}\n`,
  "utf8",
);
writeFileSync(
  join(workspaceRoot, ".sigil", "skills", "desktop-e2e-skill", "SKILL.md"),
  `---
id: desktop-e2e-skill
name: Desktop E2E Skill
description: Prove that a workspace skill is discovered, loaded, and executed.
trust: trusted
run-as: inline
user-invocable: true
allowed-tools: [read_file]
---

# Desktop skill execution

DESKTOP_E2E_SKILL_INSTRUCTION
Read the workspace README and return the fixture canary.
`,
  "utf8",
);
writeFileSync(
  join(workspaceRoot, ".sigil", "agents", "desktop-e2e-agent", "agent.toml"),
  `description = "Prove that a workspace agent is discovered and executed."
instructions = """
DESKTOP_E2E_AGENT_INSTRUCTION
Read the workspace README and return the fixture canary.
"""
trust = "trusted"
invocation_policy = "model_allowed"
allowed_tools = ["read_file"]
`,
  "utf8",
);

process.env.SIGIL_DESKTOP_E2E_ARTIFACTS = artifactRoot;

const appBinaryPath = resolve("..", "..", "target", "debug", process.platform === "win32"
  ? "sigil-desktop-app.exe"
  : "sigil-desktop-app");

export const config = {
  runner: "local",
  specs: [resolve("e2e", "features", "**", "*.feature")],
  maxInstances: 1,
  capabilities: [{
    browserName: "tauri",
    "wdio:enforceWebDriverClassic": true,
    "tauri:options": {
      application: appBinaryPath,
    },
    "wdio:tauriServiceOptions": {
      appBinaryPath,
      appArgs: [],
      driverProvider: "embedded" as const,
      env: {
        HOME: e2eHome,
        USERPROFILE: e2eHome,
        SIGIL_STATE_HOME: stateHome,
        SIGIL_CACHE_HOME: cacheHome,
      },
      captureBackendLogs: true,
      captureFrontendLogs: true,
      backendLogLevel: "info" as const,
      frontendLogLevel: "info" as const,
    },
  }],
  services: [[
    "@wdio/tauri-service",
    {
      driverProvider: "embedded" as const,
      captureBackendLogs: true,
      captureFrontendLogs: true,
    },
  ]],
  framework: "cucumber",
  reporters: ["spec"],
  logLevel: "info" as const,
  outputDir: artifactRoot,
  waitforTimeout: 20_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 2,
  autoXvfb: false,
  cucumberOpts: {
    import: [resolve("e2e", "steps", "**", "*.ts")],
    strict: true,
    failFast: false,
    timeout: 120_000,
  },
};
