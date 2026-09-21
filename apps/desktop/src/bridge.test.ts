import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
import { desktopBridge } from "./bridge";

beforeEach(() => { invoke.mockReset(); });

it("sends command-history recovery only through its narrow native command", async () => {
  invoke.mockResolvedValue({ Preview: {} });
  await desktopBridge.recoverControlLog("workspace", "session", "Preview");
  expect(invoke).toHaveBeenCalledWith("desktop_recover_control_log", { workspaceId: "workspace", sessionId: "session", action: "Preview" });
});

describe("native history observation cancellation", () => {
  it("does not prepare an already aborted query", async () => {
    const controller = new AbortController();
    controller.abort();
    await expect(desktopBridge.display("workspace", "session", {}, controller.signal)).rejects.toMatchObject({ name: "AbortError" });
    expect(invoke).not.toHaveBeenCalled();
  });

  it("revokes a prepare that completes after abort without invoking display", async () => {
    let prepared: ((id: string) => void) | undefined;
    invoke.mockImplementation((command) => command === "desktop_prepare_history_query"
      ? new Promise<string>((resolve) => { prepared = resolve; }) : Promise.resolve());
    const controller = new AbortController();
    const result = desktopBridge.display("workspace", "session", {}, controller.signal);
    const rejected = expect(result).rejects.toMatchObject({ name: "AbortError" });
    controller.abort();
    prepared?.("ticket-old");
    await rejected;
    expect(invoke.mock.calls.map(([command]) => command)).toEqual(["desktop_prepare_history_query", "desktop_cancel_history_query"]);
    expect(invoke).toHaveBeenLastCalledWith("desktop_cancel_history_query", { workspaceId: "workspace", sessionId: "session", queryId: "ticket-old" });
  });

  it.each(["display", "transcript"] as const)("cancels an in-flight %s using only its registered binding", async (kind) => {
    let failQuery: ((error: unknown) => void) | undefined;
    invoke.mockImplementation((command) => {
      if (command === "desktop_prepare_history_query") return Promise.resolve("ticket-live");
      if (command === "desktop_cancel_history_query") {
        failQuery?.({ code: "history_query_cancelled" });
        return Promise.resolve();
      }
      return new Promise((_resolve, reject) => { failQuery = reject; });
    });
    const controller = new AbortController();
    const result = desktopBridge[kind]("workspace", "session", { limit: 50 }, controller.signal);
    await vi.waitFor(() => expect(failQuery).toBeDefined());
    const rejected = expect(result).rejects.toMatchObject({ code: "history_query_cancelled" });
    controller.abort();
    await rejected;
    expect(invoke.mock.calls.filter(([command]) => command === "desktop_cancel_history_query")).toHaveLength(1);
    expect(invoke).toHaveBeenCalledWith(`desktop_${kind}`, { workspaceId: "workspace", sessionId: "session", queryId: "ticket-live", request: { limit: 50 } });
  });

  it("releases a prepared ticket when native validation rejects the query", async () => {
    invoke.mockImplementation((command) => {
      if (command === "desktop_prepare_history_query") return Promise.resolve("ticket-invalid");
      if (command === "desktop_display") return Promise.reject({ code: "conversation_display_query_invalid" });
      return Promise.resolve();
    });
    await expect(desktopBridge.display("workspace", "session", { limit: 0 })).rejects.toMatchObject({ code: "conversation_display_query_invalid" });
    expect(invoke).toHaveBeenLastCalledWith("desktop_cancel_history_query", { workspaceId: "workspace", sessionId: "session", queryId: "ticket-invalid" });
  });

  it("cancels a full message page through the same scope-bound native ticket", async () => {
    let failQuery: ((error: unknown) => void) | undefined;
    invoke.mockImplementation((command) => {
      if (command === "desktop_prepare_history_query") return Promise.resolve("message-ticket");
      if (command === "desktop_cancel_history_query") {
        failQuery?.({ code: "history_query_cancelled" });
        return Promise.resolve();
      }
      return new Promise((_resolve, reject) => { failQuery = reject; });
    });
    const controller = new AbortController();
    const request = { displayId: "saved-message", offset: 65_536, contentVersion: "v1" };
    const result = desktopBridge.messageContent("workspace", "session", request, controller.signal);
    const rejected = expect(result).rejects.toMatchObject({ code: "history_query_cancelled" });
    await vi.waitFor(() => expect(failQuery).toBeDefined());
    controller.abort();
    await rejected;
    expect(invoke).toHaveBeenCalledWith("desktop_message_content", {
      workspaceId: "workspace", sessionId: "session", queryId: "message-ticket", request,
    });
    expect(invoke.mock.calls.filter(([command]) => command === "desktop_cancel_history_query")).toHaveLength(1);
  });
});
