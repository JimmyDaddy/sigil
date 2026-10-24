import type { ProviderConnection, ProviderModelRef } from "../../types";

export type ModelRouteIssue =
  | "customModelConnectionUnavailable"
  | "customModelIdRequired"
  | "customModelIdTooLong"
  | "customModelIdInvalid";

/** Local input validation only; the runtime resolves the live configured route at admission. */
export function configuredModelRouteIssue(
  model: ProviderModelRef,
  connections: readonly ProviderConnection[],
): ModelRouteIssue | undefined {
  if (!connections.some((connection) => connection.id === model.connectionId)) {
    return "customModelConnectionUnavailable";
  }
  if (model.modelId.trim() === "") return "customModelIdRequired";
  if (new TextEncoder().encode(model.modelId).byteLength > 256) return "customModelIdTooLong";
  if (model.modelId.trim() !== model.modelId
    || /[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/u.test(model.modelId)) {
    return "customModelIdInvalid";
  }
  return undefined;
}
