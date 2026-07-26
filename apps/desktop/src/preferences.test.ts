import { afterEach, describe, expect, it } from "vitest";

import { readDefaultModel, writeDefaultModel } from "./preferences";

afterEach(() => {
  window.localStorage.clear();
});

describe("desktop provider model preferences", () => {
  it("round-trips an exact compound model identity", () => {
    expect(writeDefaultModel("workspace-1", {
      connectionId: "openai-primary",
      modelId: "gpt-4.1",
    })).toBe(true);

    expect(readDefaultModel("workspace-1")).toEqual({
      connectionId: "openai-primary",
      modelId: "gpt-4.1",
    });
  });

  it("does not guess a connection for a legacy bare model preference", () => {
    window.localStorage.setItem(
      "sigil.desktop.default-models.v1",
      JSON.stringify({ "workspace-1": "deepseek-v4-pro" }),
    );

    expect(readDefaultModel("workspace-1")).toBeUndefined();
  });

  it("rejects malformed compound identities", () => {
    window.localStorage.setItem(
      "sigil.desktop.default-models.v2",
      JSON.stringify({
        "workspace-1": {
          connectionId: "",
          modelId: "gpt-4.1",
        },
      }),
    );

    expect(readDefaultModel("workspace-1")).toBeUndefined();
  });
});
