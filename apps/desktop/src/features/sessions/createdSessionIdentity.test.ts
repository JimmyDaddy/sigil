import { describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { CatalogPage } from "../../types";
import { findCreatedSessionEntry } from "./createdSessionIdentity";

const emptyPage: CatalogPage = {
  workspaceId: "workspace-current", generation: 1, reconciledAtUnixMs: 1,
  degradedSourceCount: 0, identityConflictCount: 0, truncatedSourceCount: 0, entries: [],
};

const createdEntry = {
  sessionRef: "created.jsonl", sessionId: "durable-created", sourceState: "ready" as const,
  sourceBytes: 512, sourceModifiedAtUnixMs: 1, userMessageCount: 0,
  assistantMessageCount: 0, toolResultCount: 0, pinned: false,
};

describe("created conversation identity boundary", () => {
  it("does not accept a catalog page from another workspace", async () => {
    const catalog = vi.fn<DesktopBridge["catalog"]>(async () => ({
      ...emptyPage, workspaceId: "workspace-other", entries: [createdEntry],
    }));
    expect(await findCreatedSessionEntry({ catalog }, emptyPage.workspaceId, "durable-created", () => true)).toBeUndefined();
    expect(catalog).toHaveBeenCalledOnce();
  });

  it("stops an already superseded selection without requesting another page", async () => {
    let current = true;
    const catalog = vi.fn<DesktopBridge["catalog"]>(async () => {
      current = false;
      return { ...emptyPage, nextCursor: "next" };
    });
    expect(await findCreatedSessionEntry({ catalog }, emptyPage.workspaceId, "durable-created", () => current)).toBeUndefined();
    expect(catalog).toHaveBeenCalledOnce();
  });

  it.each(["repeated", "unending"] as const)("bounds a %s cursor chain", async (chain) => {
    let sequence = 0;
    const catalog = vi.fn<DesktopBridge["catalog"]>(async () => ({
      ...emptyPage, nextCursor: chain === "repeated" ? "same-page" : `page-${++sequence}`,
    }));
    expect(await findCreatedSessionEntry({ catalog }, emptyPage.workspaceId, "durable-created", () => true)).toBeUndefined();
    expect(catalog.mock.calls.length).toBe(chain === "repeated" ? 2 : 10);
  });

  it("does not remember an exact identity whose durable source is not reopenable", async () => {
    const catalog = vi.fn<DesktopBridge["catalog"]>(async () => ({
      ...emptyPage, entries: [{ ...createdEntry, sourceState: "invalid" }],
    }));
    expect(await findCreatedSessionEntry({ catalog }, emptyPage.workspaceId, "durable-created", () => true)).toBeUndefined();
  });
});
