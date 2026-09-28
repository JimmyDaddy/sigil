import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { ProviderSetupCatalog, ProviderSetupCatalogInput } from "../../types";
import {
  loadAndCacheProviderCatalog,
  readProviderCatalogCache,
} from "./providerCatalogCache";

describe("provider catalog view cache", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
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

  it("keeps a stale catalog through an offline error but removes it after authentication is rejected", async () => {
    let now = 1_784_505_600_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    let response: ProviderSetupCatalog = {
      connectionId: "deepseek-revoked-1",
      providerLabel: "DeepSeek",
      state: "remote",
      models: [{
        modelId: "deepseek-coder",
        displayName: "DeepSeek Coder",
        availability: "available",
        recommended: true,
        provenance: "remote",
      }],
      suggestedModel: "deepseek-coder",
      manualEntryAllowed: true,
    };
    const bridge = {
      providerSetupCatalog: vi.fn(async () => response),
    } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "deep_seek",
      credentialSource: "environment",
    };
    const workspaceId = "revoked-cache-workspace-test";

    const original = await loadAndCacheProviderCatalog(bridge, workspaceId, input);
    now += 10 * 60 * 1_000 + 1;
    response = { ...original, state: "offline", models: [] };
    await loadAndCacheProviderCatalog(bridge, workspaceId, input);
    expect(await readProviderCatalogCache(workspaceId, input))
      .toEqual({ catalog: original, stale: true });

    response = { ...original, state: "auth_rejected", models: [] };
    await loadAndCacheProviderCatalog(bridge, workspaceId, input);
    expect(await readProviderCatalogCache(workspaceId, input)).toBeUndefined();
  });

  it("does not restore an older remote catalog after a newer authentication rejection", async () => {
    const pending: Array<(catalog: ProviderSetupCatalog) => void> = [];
    const providerSetupCatalog = vi.fn(() => new Promise<ProviderSetupCatalog>((resolve) => {
      pending.push(resolve);
    }));
    const bridge = { providerSetupCatalog } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "deep_seek",
      credentialSource: "environment",
    };
    const workspaceId = "out-of-order-auth-cache-workspace-test";
    const remote: ProviderSetupCatalog = {
      connectionId: "deepseek-out-of-order-1",
      providerLabel: "DeepSeek",
      state: "remote",
      models: [{
        modelId: "old-remote-model",
        displayName: "Old Remote Model",
        availability: "available",
        recommended: true,
        provenance: "remote",
      }],
      manualEntryAllowed: true,
    };

    const older = loadAndCacheProviderCatalog(bridge, workspaceId, input);
    await vi.waitFor(() => expect(pending).toHaveLength(1));
    const newer = loadAndCacheProviderCatalog(bridge, workspaceId, input);
    await vi.waitFor(() => expect(pending).toHaveLength(2));
    pending[1]?.({ ...remote, state: "auth_rejected", models: [] });
    expect((await newer).state).toBe("auth_rejected");
    pending[0]?.(remote);
    expect((await older).state).toBe("remote");

    expect(await readProviderCatalogCache(workspaceId, input)).toBeUndefined();
    expect(providerSetupCatalog).toHaveBeenCalledTimes(2);
  });

  it("uses call order when an older credential digest completes after a newer rejected request", async () => {
    const digests: Array<(value: ArrayBuffer) => void> = [];
    vi.stubGlobal("crypto", {
      subtle: {
        digest: () => new Promise<ArrayBuffer>((resolve) => {
          digests.push(resolve);
        }),
      },
    });
    const pending: Array<(catalog: ProviderSetupCatalog) => void> = [];
    const bridge = {
      providerSetupCatalog: vi.fn(() => new Promise<ProviderSetupCatalog>((resolve) => {
        pending.push(resolve);
      })),
    } as unknown as DesktopBridge;
    const input: ProviderSetupCatalogInput = {
      template: "deep_seek",
      credentialSource: "secure_store",
      apiKey: "same-revoked-key",
    };
    const workspaceId = "delayed-digest-cache-workspace-test";
    const remote: ProviderSetupCatalog = {
      connectionId: "deepseek-delayed-1",
      providerLabel: "DeepSeek",
      state: "remote",
      models: [{
        modelId: "old-remote-model",
        displayName: "Old Remote Model",
        availability: "available",
        recommended: true,
        provenance: "remote",
      }],
      manualEntryAllowed: true,
    };
    const hash = new Uint8Array([1, 2, 3]).buffer;

    const older = loadAndCacheProviderCatalog(bridge, workspaceId, input);
    const newer = loadAndCacheProviderCatalog(bridge, workspaceId, input);
    expect(digests).toHaveLength(2);
    digests[1]?.(hash);
    await vi.waitFor(() => expect(pending).toHaveLength(1));
    pending[0]?.({ ...remote, state: "auth_rejected", models: [] });
    expect((await newer).state).toBe("auth_rejected");

    digests[0]?.(hash);
    await vi.waitFor(() => expect(pending).toHaveLength(2));
    pending[1]?.(remote);
    expect((await older).state).toBe("remote");
    const cached = readProviderCatalogCache(workspaceId, input);
    expect(digests).toHaveLength(3);
    digests[2]?.(hash);
    expect(await cached).toBeUndefined();
  });

  it("keeps different request keys independent when their responses arrive out of order", async () => {
    const pending: Array<(catalog: ProviderSetupCatalog) => void> = [];
    const bridge = {
      providerSetupCatalog: vi.fn(() => new Promise<ProviderSetupCatalog>((resolve) => {
        pending.push(resolve);
      })),
    } as unknown as DesktopBridge;
    const workspaceId = "parallel-cache-keys-workspace-test";
    const firstInput: ProviderSetupCatalogInput = {
      template: "open_ai_compatible",
      protocol: "chat_completions",
      endpoint: "http://127.0.0.1:11450/v1",
      credentialSource: "none",
    };
    const secondInput = { ...firstInput, endpoint: "http://127.0.0.1:11451/v1" };
    const remote: ProviderSetupCatalog = {
      connectionId: "local-parallel-1",
      providerLabel: "OpenAI-compatible",
      state: "remote",
      models: [],
      manualEntryAllowed: true,
    };

    const first = loadAndCacheProviderCatalog(bridge, workspaceId, firstInput);
    await vi.waitFor(() => expect(pending).toHaveLength(1));
    const second = loadAndCacheProviderCatalog(bridge, workspaceId, secondInput);
    await vi.waitFor(() => expect(pending).toHaveLength(2));
    pending[1]?.({ ...remote, state: "auth_rejected" });
    await second;
    pending[0]?.(remote);
    await first;

    expect(await readProviderCatalogCache(workspaceId, firstInput))
      .toEqual({ catalog: remote, stale: false });
    expect(await readProviderCatalogCache(workspaceId, secondInput)).toBeUndefined();
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
