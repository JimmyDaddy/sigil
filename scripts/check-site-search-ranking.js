#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");

const repoRoot = path.resolve(__dirname, "..");
const indexPath = path.resolve(process.argv[2] || path.join(repoRoot, "_site/search.json"));
const policyPath = path.resolve(
  process.argv[3] || path.join(repoRoot, "dev/docs/public-documentation-content-policy.json")
);
const ranking = require(path.join(repoRoot, "site/assets/search-ranking.js"));

const index = JSON.parse(fs.readFileSync(indexPath, "utf8"));
const policy = JSON.parse(fs.readFileSync(policyPath, "utf8"));
const failures = [];
const expectedCases = {
  en: {
    install: "installation",
    provider: "providers",
    approval: "safety",
    sandbox: "permissions-and-sandbox",
    MCP: "mcp",
    "session restore": "user-guide",
  },
  "zh-CN": {
    安装: "installation",
    provider: "providers",
    审批: "safety",
    沙箱: "permissions-and-sandbox",
    MCP: "mcp",
    会话恢复: "user-guide",
  },
};

if (JSON.stringify(policy.search_first_result) !== JSON.stringify(expectedCases)) {
  failures.push("policy search_first_result must match the fixed EN/ZH authority query inventory");
}

for (const [locale, cases] of Object.entries(expectedCases)) {
  for (const [query, slug] of Object.entries(cases)) {
    const results = ranking.rank(index, query, locale, 8);
    const expected = `${locale === "en" ? "docs" : "zh-CN/docs"}/${slug}/`;
    const actual = results[0] && results[0].url.split("#", 1)[0];
    if (actual !== expected) {
      failures.push(`${locale} ${JSON.stringify(query)}: expected ${expected}, found ${actual || "no result"}`);
    }
    if (!results[0] || results[0].kind !== "page") {
      failures.push(`${locale} ${JSON.stringify(query)}: authority result must be a page item`);
    }
  }
}

if (failures.length > 0) {
  process.stderr.write(`search ranking checks failed:\n${failures.join("\n")}\n`);
  process.exit(1);
}

process.stdout.write("search ranking checks passed\n");
