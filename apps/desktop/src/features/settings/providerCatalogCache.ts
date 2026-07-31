import type { DesktopBridge } from "../../bridge";
import type {
  ProviderSetupCatalog,
  ProviderSetupCatalogInput,
} from "../../types";

const FRESH_TTL_MS = 10 * 60 * 1_000;
const STALE_TTL_MS = 24 * 60 * 60 * 1_000;
const MAX_ENTRIES = 32;

interface CachedCatalog {
  readonly catalog: ProviderSetupCatalog;
  readonly storedAt: number;
}

export interface ProviderCatalogCacheHit {
  readonly catalog: ProviderSetupCatalog;
  readonly stale: boolean;
}

const cache = new Map<string, CachedCatalog>();

async function requestCacheKey(
  workspaceId: string,
  input: ProviderSetupCatalogInput,
): Promise<string | undefined> {
  let credentialFingerprint = "no-key";
  if (input.apiKey !== undefined) {
    if (globalThis.crypto?.subtle === undefined) return undefined;
    const bytes = new TextEncoder().encode(input.apiKey);
    const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
    credentialFingerprint = Array.from(new Uint8Array(digest))
      .map((value) => value.toString(16).padStart(2, "0"))
      .join("");
  }
  return JSON.stringify([
    workspaceId,
    input.template,
    input.protocol ?? "",
    input.endpoint?.trim() ?? "",
    input.credentialSource,
    input.replaceInvalidConfig ?? false,
    credentialFingerprint,
  ]);
}

export async function readProviderCatalogCache(
  workspaceId: string,
  input: ProviderSetupCatalogInput,
): Promise<ProviderCatalogCacheHit | undefined> {
  const key = await requestCacheKey(workspaceId, input);
  if (key === undefined) return undefined;
  const entry = cache.get(key);
  if (entry === undefined) return undefined;
  const age = Date.now() - entry.storedAt;
  if (age > STALE_TTL_MS) {
    cache.delete(key);
    return undefined;
  }
  return { catalog: entry.catalog, stale: age > FRESH_TTL_MS };
}

export async function loadAndCacheProviderCatalog(
  bridge: DesktopBridge,
  workspaceId: string,
  input: ProviderSetupCatalogInput,
): Promise<ProviderSetupCatalog> {
  const catalog = await bridge.providerSetupCatalog(workspaceId, input);
  const key = await requestCacheKey(workspaceId, input);
  if (key !== undefined && catalogIsReusable(catalog)) {
    cache.delete(key);
    cache.set(key, { catalog, storedAt: Date.now() });
    while (cache.size > MAX_ENTRIES) {
      const oldest = cache.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      cache.delete(oldest);
    }
  }
  return catalog;
}

function catalogIsReusable(catalog: ProviderSetupCatalog): boolean {
  return catalog.state === "remote"
    || catalog.state === "cache_fresh"
    || catalog.state === "remote_empty"
    || catalog.state === "catalog_unsupported";
}
