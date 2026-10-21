import { describe, expect, it } from "vitest";

import {
  connectionMatchesProvider,
  providerConnectionDetail,
  providerConnectionLabel,
} from "./providerPresentation";

describe("provider connection presentation", () => {
  it("shows an identical connection and provider once after trimming", () => {
    const connection = { label: " DeepSeek ", providerLabel: "DeepSeek" };
    expect(connectionMatchesProvider(connection)).toBe(true);
    expect(providerConnectionLabel(connection)).toBe("DeepSeek");
    expect(providerConnectionDetail({ ...connection, protocolLabel: " DeepSeek " })).toBe("");
    expect(providerConnectionDetail({ ...connection, protocolLabel: "Chat Completions" }))
      .toBe("Chat Completions");
  });

  it.each(["Team gateway", "DeepSeek local", "deepseek"])(
    "retains both identities when the connection is %s",
    (label) => {
      const connection = { label, providerLabel: "DeepSeek" };
      expect(connectionMatchesProvider(connection)).toBe(false);
      expect(providerConnectionLabel(connection)).toBe(`DeepSeek · ${label}`);
      expect(providerConnectionDetail({ ...connection, protocolLabel: "Chat Completions" }))
        .toBe("DeepSeek · Chat Completions");
    },
  );

  it("retains a distinct provider and shows an identical protocol only once", () => {
    expect(providerConnectionDetail({
      label: "Team gateway",
      providerLabel: "DeepSeek",
      protocolLabel: " DeepSeek ",
    })).toBe("DeepSeek");
  });
});
