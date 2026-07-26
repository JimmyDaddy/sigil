import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { ProviderSetupCatalogInput } from "../../types";
import {
  loadAndCacheProviderCatalog,
  readProviderCatalogCache,
} from "./providerCatalogCache";

describe("provider catalog view cache", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("reuses a fresh exact request without another provider load", async () => {
    const providerSetupCatalog = vi.fn(async () => ({
      connectionId: "local-1",
      providerLabel: "OpenAI-compatible",
      state: "remote",
      models: [{
        modelId: "local-coder",
        displayName: "Local Coder",
        availability: "available" as const,
        recommended: true,
        provenance: "remote" as const,
      }],
      suggestedModel: "local-coder",
      manualEntryAllowed: true,
    }));
    const bridge = { providerSetupCatalog } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "open_ai_compatible",
      protocol: "chat_completions",
      endpoint: "http://127.0.0.1:11434/v1",
      credentialSource: "none",
    };

    const loaded = await loadAndCacheProviderCatalog(
      bridge,
      "cache-workspace-test",
      input,
    );
    const cached = await readProviderCatalogCache("cache-workspace-test", input);

    expect(cached).toEqual({ catalog: loaded, stale: false });
    expect(providerSetupCatalog).toHaveBeenCalledTimes(1);
  });

  it("does not reuse authentication or transport failures", async () => {
    const providerSetupCatalog = vi.fn(async () => ({
      connectionId: "deepseek-1",
      providerLabel: "DeepSeek",
      state: "auth_rejected",
      models: [],
      manualEntryAllowed: false,
    }));
    const bridge = { providerSetupCatalog } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "deep_seek",
      credentialSource: "environment",
    };

    await loadAndCacheProviderCatalog(bridge, "failed-cache-workspace-test", input);

    expect(await readProviderCatalogCache("failed-cache-workspace-test", input))
      .toBeUndefined();
  });

  it("marks an exact catalog stale after ten minutes while keeping it visible", async () => {
    let now = 1_784_505_600_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const providerSetupCatalog = vi.fn(async () => ({
      connectionId: "local-stale-1",
      providerLabel: "OpenAI-compatible",
      state: "remote",
      models: [{
        modelId: "local-stale-coder",
        displayName: "Local Stale Coder",
        availability: "available" as const,
        recommended: true,
        provenance: "remote" as const,
      }],
      suggestedModel: "local-stale-coder",
      manualEntryAllowed: true,
    }));
    const bridge = { providerSetupCatalog } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "open_ai_compatible",
      protocol: "chat_completions",
      endpoint: "http://127.0.0.1:11436/v1",
      credentialSource: "none",
    };
    await loadAndCacheProviderCatalog(bridge, "stale-cache-workspace-test", input);

    now += 10 * 60 * 1_000 + 1;

    expect(await readProviderCatalogCache("stale-cache-workspace-test", input))
      .toMatchObject({ stale: true });
  });

  it("reuses stable empty catalogs instead of repeating discovery", async () => {
    const providerSetupCatalog = vi.fn(async () => ({
      connectionId: "local-empty-1",
      providerLabel: "OpenAI-compatible",
      state: "remote_empty",
      models: [],
      manualEntryAllowed: true,
    }));
    const bridge = { providerSetupCatalog } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "open_ai_compatible",
      protocol: "chat_completions",
      endpoint: "http://127.0.0.1:11437/v1",
      credentialSource: "none",
    };

    await loadAndCacheProviderCatalog(bridge, "empty-cache-workspace-test", input);

    expect(await readProviderCatalogCache("empty-cache-workspace-test", input))
      .toMatchObject({ stale: false });
  });
});
