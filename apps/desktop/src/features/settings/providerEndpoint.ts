import type { ProviderSetupCredentialSource } from "../../types";

/** Structural connection rules only; reachability remains owned by the service. */
export function providerEndpointIssue(raw: string, credential: ProviderSetupCredentialSource) {
  if (raw.trim() === "") return "providerEndpointRequired" as const;
  let url: URL;
  try { url = new URL(raw.trim()); } catch { return "providerEndpointInvalid" as const; }
  if (!url.hostname || !["http:", "https:"].includes(url.protocol)) return "providerEndpointInvalid" as const;
  if (url.username || url.password) return "providerEndpointUserinfo" as const;
  if (url.search || url.hash) return "providerEndpointSuffix" as const;
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if (credential === "none" && !loopback) return "providerEndpointLoopback" as const;
  if (url.protocol === "http:" && (credential !== "none" || !loopback)) return "providerEndpointHttps" as const;
  return undefined;
}
