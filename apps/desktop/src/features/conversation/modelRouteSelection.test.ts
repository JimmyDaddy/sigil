import { describe, expect, it } from "vitest";

import type { ProviderConnection } from "../../types";
import { configuredModelRouteIssue } from "./modelRouteSelection";

const connections: ProviderConnection[] = [{
  id: "custom-env", label: "Custom gateway", providerLabel: "Custom", protocolLabel: "Chat Completions",
  endpointDisplay: "gateway.example", credentialSource: "environment", readiness: "credential_unavailable",
}];

describe("explicit configured model routes", () => {
  it("admits an unlisted model without treating catalog or credential observations as permission", () => {
    expect(configuredModelRouteIssue({ connectionId: "custom-env", modelId: "unlisted/model-v2" }, connections)).toBeUndefined();
    expect(configuredModelRouteIssue({ connectionId: "custom-env", modelId: "a".repeat(256) }, connections)).toBeUndefined();
  });

  it("rejects unknown or removed connections rather than substituting a sibling", () => {
    const model = { connectionId: "missing-connection", modelId: "unlisted/model-v2" };
    expect(configuredModelRouteIssue(model, connections)).toBe("customModelConnectionUnavailable");
    expect(configuredModelRouteIssue({ ...model, connectionId: "custom-env" }, [])).toBe("customModelConnectionUnavailable");
  });

  it.each([
    ["", "customModelIdRequired"],
    ["   ", "customModelIdRequired"],
    ["a".repeat(257), "customModelIdTooLong"],
    ["模".repeat(86), "customModelIdTooLong"],
    ["model\u001b[31m", "customModelIdInvalid"],
    ["model\u009d", "customModelIdInvalid"],
    ["model\u202e", "customModelIdInvalid"],
    ["model\u2066", "customModelIdInvalid"],
    [" model ", "customModelIdInvalid"],
  ])("rejects malformed model identities (%s)", (modelId, issue) => {
    expect(configuredModelRouteIssue({ connectionId: "custom-env", modelId }, connections)).toBe(issue);
  });
});
