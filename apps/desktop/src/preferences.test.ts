import { afterEach, describe, expect, it } from "vitest";

import { readLastSession, writeLastSession } from "./preferences";

afterEach(() => {
  window.localStorage.clear();
});

describe("desktop session preferences", () => {
  it("stores the last selected durable session per workspace", () => {
    expect(writeLastSession("workspace-a", {
      sessionRef: "session-a.jsonl",
      sessionId: "durable-a",
      label: "Last conversation",
    })).toBe(true);
    expect(writeLastSession("workspace-b", {
      sessionRef: "session-b.jsonl",
      sessionId: "durable-b",
    })).toBe(true);

    expect(readLastSession("workspace-a")).toEqual({
      sessionRef: "session-a.jsonl",
      sessionId: "durable-a",
      label: "Last conversation",
    });
    expect(readLastSession("workspace-b")).toEqual({
      sessionRef: "session-b.jsonl",
      sessionId: "durable-b",
    });
  });

  it("rejects malformed stored values and clears only the requested workspace", () => {
    window.localStorage.setItem("sigil.desktop.last-sessions.v1", JSON.stringify({
      "workspace-a": { sessionRef: 42, sessionId: "durable-a" },
      "workspace-b": { sessionRef: "session-b.jsonl", sessionId: "durable-b" },
    }));

    expect(readLastSession("workspace-a")).toBeUndefined();
    expect(writeLastSession("workspace-b", undefined)).toBe(true);
    expect(readLastSession("workspace-b")).toBeUndefined();
  });
});
