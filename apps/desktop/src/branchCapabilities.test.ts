import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { expect, it } from "vitest";

it("allows branch reads through the enabled narrow conversation recovery permission", () => {
  const root = resolve(process.cwd(), "src-tauri");
  const capability = JSON.parse(readFileSync(resolve(root, "capabilities/main.json"), "utf8")) as { permissions: string[] };
  expect(capability.permissions).toContain("allow-desktop-conversation-recovery");
  const permissions = readFileSync(resolve(root, "permissions/desktop.toml"), "utf8");
  const recovery = permissions.split("[[permission]]").find((entry) =>
    entry.includes('identifier = "allow-desktop-conversation-recovery"'));
  expect(recovery).toBeDefined();
  const commands = recovery?.match(/commands\.allow = \[([^\]]+)\]/)?.[1].match(/"[^"]+"/g);
  expect(commands).toContain('"desktop_branch_lineage"');
  expect(commands).toContain('"desktop_branch_knowledge_preview"');
  const native = readFileSync(resolve(root, "src/lib.rs"), "utf8");
  const handler = native.slice(native.indexOf(".invoke_handler(tauri::generate_handler!["));
  expect(handler).toContain("desktop_branch_lineage,");
  expect(handler).toContain("desktop_branch_knowledge_preview,");
});
