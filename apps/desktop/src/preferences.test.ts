import { afterEach, describe, expect, it } from "vitest";

import { readLastSession, writeLastSession } from "./preferences";

afterEach(() => {
  window.localStorage.clear();
});

describe("desktop session preferences", () => {
  it("round-trips an exact durable session identity", () => {
    expect(writeLastSession("workspace-1", {
      sessionRef: "session-1.jsonl",
      sessionId: "session-1",
      label: "Provider switching",
    })).toBe(true);

    expect(readLastSession("workspace-1")).toEqual({
      sessionRef: "session-1.jsonl",
      sessionId: "session-1",
      label: "Provider switching",
    });
  });

  it("rejects malformed durable session identities", () => {
    window.localStorage.setItem(
      "sigil.desktop.last-sessions.v1",
      JSON.stringify({
        "workspace-1": {
          sessionRef: "",
          sessionId: "session-1",
        },
      }),
    );

    expect(readLastSession("workspace-1")).toBeUndefined();
  });
});
