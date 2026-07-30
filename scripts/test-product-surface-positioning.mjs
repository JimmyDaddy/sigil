#!/usr/bin/env node

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const activePositioningFiles = [
  "AGENTS.md",
  "CONTRIBUTING.md",
  "README.md",
  "README.zh-CN.md",
  ".github/ISSUE_TEMPLATE/feature-request.yml",
  "crates/sigil/src/main.rs",
  "dev/docs/desktop-dogfood.md",
  "dev/docs/index.md",
  "dev/docs/sigil-capability-roadmap.md",
  "dev/docs/sigil-rust-agent-core-technical-solution.md",
  "dev/governance/code-standards.md",
  "dev/governance/dependency-supply-chain.md",
  "dev/governance/engineering-standards.md",
  "docs/en/status.md",
  "docs/zh-CN/status.md",
  "scripts/prepare-npm-packages.sh",
  "scripts/render-homebrew-formula.sh",
  "site/assets/screenshots/tui-session.svg",
  "assets/social/sigil-social-preview.svg",
  "site/index.html",
  "site/zh-CN/index.html",
];

for (const relativePath of activePositioningFiles) {
  const content = readFileSync(resolve(root, relativePath), "utf8");
  assert.doesNotMatch(
    content,
    /\bTUI[- ]first\b|TUI 是第一|第一用户表面|以 TUI 为核心/i,
    `${relativePath} must not restore a TUI-first product hierarchy`,
  );
}

const agentInstructions = readFileSync(resolve(root, "AGENTS.md"), "utf8");
assert.match(
  agentInstructions,
  /Desktop 与 TUI 是并列的一等产品表面/,
  "workspace system memory must describe Desktop and TUI as peer product surfaces",
);
assert.match(
  agentInstructions,
  /不要把某个 UI 入口、实现语言或内部架构写成 agent 的能力声明或自我介绍/,
  "workspace system memory must reject UI implementation details as capability claims",
);

const basePrompt = readFileSync(
  resolve(root, "crates/sigil-kernel/src/memory.rs"),
  "utf8",
);
assert.doesNotMatch(basePrompt, /\bTUI[- ]first\b/i);
assert.match(basePrompt, /do not turn implementation details, UI entrypoints/);

const englishHome = readFileSync(resolve(root, "site/index.html"), "utf8");
const chineseHome = readFileSync(resolve(root, "site/zh-CN/index.html"), "utf8");
const siteScript = readFileSync(resolve(root, "site/assets/site.js"), "utf8");
assert.match(englishHome, /Desktop \+ TUI coding agent/);
assert.match(chineseHome, /Desktop \+ TUI 编码智能体/);
for (const [label, home] of [
  ["English homepage", englishHome],
  ["Chinese homepage", chineseHome],
]) {
  assert.match(
    home,
    /sigil-desktop-demo\.(?:webm|mp4)/,
    `${label} must expose a Desktop demo`,
  );
  assert.match(
    home,
    /sigil-45-second-demo\.(?:webm|mp4)/,
    `${label} must retain the TUI real-run demo`,
  );
  assert.match(
    home,
    /aarch64/,
    `${label} must identify the Apple Silicon Desktop asset`,
  );
  assert.match(
    home,
    /x86_64-apple-darwin/,
    `${label} must identify the Intel Desktop asset`,
  );
}
assert.match(englishHome, /data-desktop-update-platform="darwin-aarch64"/);
assert.match(englishHome, /data-desktop-update-platform="darwin-x86_64"/);
assert.match(chineseHome, /data-desktop-update-platform="darwin-aarch64"/);
assert.match(chineseHome, /data-desktop-update-platform="darwin-x86_64"/);
assert.match(siteScript, /updates?\/beta|desktopUpdateManifest/);
assert.match(siteScript, /archiveUrl\.hostname !== "github\.com"/);
assert.match(siteScript, /\/JimmyDaddy\/sigil\/releases\/download\//);
assert.match(siteScript, /\.app\\\.tar\\\.gz\$/, "Desktop download resolution must derive the DMG from the signed updater archive");

console.log("product surface positioning contract: ok");
