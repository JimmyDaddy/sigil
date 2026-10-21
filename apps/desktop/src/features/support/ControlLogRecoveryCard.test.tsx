import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { ControlLogRecoveryPreview } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
import { ControlLogRecoveryCard } from "./ControlLogRecoveryCard";
import { SupportPage } from "./SupportPage";

const preview: ControlLogRecoveryPreview = {
  authority: {
  request: {
    logical_journal_id: "a".repeat(64), operation_id: "selected-recovery",
    from_generation: 0, successor_generation: 1, header_digest: "b".repeat(64), owner_context_digest: "f".repeat(64),
  },
  old_namespace_hash: "a".repeat(64), successor_namespace_hash: "c".repeat(64),
  old_byte_length: 721, old_content_digest: "d".repeat(64), preview_digest: "e".repeat(64),
  old_file_identity: "f".repeat(64),
  },
  impact: {
    verified_prefix_bytes: 700, verified_record_count: 3, verified_prefix_digest: "a".repeat(64),
    known_command_count: 40, affected_scope_count: 20,
    affected_scopes: [{ scope_digest: "b".repeat(64), session_id: "affected-session", workspace_id: "workspace" }],
    scopes_truncated: true, known_unresolved_count: 35,
    unresolved_commands: [{ key_digest: "c".repeat(64), scope_digest: "b".repeat(64), command_id: "pending-command", command_kind: "SelectRoute", phase: "Uncertain" }],
    commands_truncated: true, unparsed_tail_bytes: 21, tail_command_count_unknown: true,
  },
};
afterEach(cleanup);

describe("command history recovery", () => {
  it("exports support even when the separate diagnostic preview is unavailable", async () => {
    const exportSupportBundle = vi.fn().mockResolvedValue({ cancelled: false, fileName: "sigil-support.json" });
    const bridge = {
      supportDoctor: vi.fn().mockRejectedValue(new Error("preview unavailable")),
      exportSupportBundle,
    } as unknown as DesktopBridge;
    render(<NotificationProvider><SupportPage bridge={bridge} workspaceId="export-workspace" onBack={() => undefined} /></NotificationProvider>);
    await screen.findByRole("heading", { name: "Diagnostics are unavailable" });
    const save = screen.getByRole("button", { name: "Save private report" });
    expect(save.hasAttribute("disabled")).toBe(false);
    fireEvent.click(save);
    await waitFor(() => expect(exportSupportBundle).toHaveBeenCalledWith("export-workspace", []));
  });
  it("remains available when diagnostics fail and retries the exact preview after confirmation failure", async () => {
    const recoverControlLog = vi.fn()
      .mockResolvedValueOnce({ Preview: preview })
      .mockRejectedValueOnce(new Error("directory sync failed"))
      .mockResolvedValueOnce({ Activated: { logical_journal_id: preview.authority.request.logical_journal_id, command_generation: 1 } });
    const bridge = {
      supportDoctor: vi.fn().mockRejectedValue(new Error("unavailable")), recoverControlLog,
    } as unknown as DesktopBridge;
    render(<NotificationProvider><SupportPage bridge={bridge} workspaceId="workspace" session={{ id: "session", label: "Existing conversation", runCount: 0 }} onBack={() => undefined} /></NotificationProvider>);
    await screen.findByRole("heading", { name: "Diagnostics are unavailable" });
    fireEvent.click(screen.getByText("Command history recovery", { selector: "summary span" }));
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await screen.findByText("721");
    expect(screen.getByText(preview.authority.old_content_digest)).toBeTruthy();
    expect(screen.getByText("3 records, 700 bytes")).toBeTruthy();
    expect(screen.getByText("35")).toBeTruthy();
    expect(screen.getByText("affected-session")).toBeTruthy();
    expect(screen.getByText("pending-command")).toBeTruthy();
    expect(screen.getByText(/Additional commands in this tail are unknown/)).toBeTruthy();
    expect(screen.getByText(/Only the first 32 unresolved commands/)).toBeTruthy();
    expect(recoverControlLog).toHaveBeenCalledTimes(1);
    expect(recoverControlLog).toHaveBeenCalledWith("workspace", "session", "Preview");
    fireEvent.click(screen.getByRole("button", { name: "Confirm recovery" }));
    await screen.findByText(/Recovery could not finish/);
    expect(recoverControlLog).toHaveBeenLastCalledWith("workspace", "session", { SealAndRotate: { preview } });
    fireEvent.click(screen.getByRole("button", { name: "Confirm recovery" }));
    await screen.findByText("Command history generation 1 is ready.");
    expect(recoverControlLog.mock.calls[2]).toEqual(recoverControlLog.mock.calls[1]);
  });

  it("discards a preview when the selected conversation changes", async () => {
    const bridge = {
      supportDoctor: vi.fn().mockRejectedValue(new Error("unavailable")),
      recoverControlLog: vi.fn().mockResolvedValue({ Preview: preview }),
    } as unknown as DesktopBridge;
    const page = (id: string) => <NotificationProvider><SupportPage bridge={bridge} workspaceId="workspace" session={{ id, runCount: 0 }} onBack={() => undefined} /></NotificationProvider>;
    const { rerender } = render(page("first"));
    fireEvent.click(screen.getByText("Command history recovery", { selector: "summary span" }));
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await screen.findByRole("button", { name: "Confirm recovery" });
    rerender(page("second"));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Confirm recovery" })).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await waitFor(() => expect(bridge.recoverControlLog).toHaveBeenLastCalledWith("workspace", "second", "Preview"));
  });
});

describe("typed recovery feedback and ownership", () => {
  it.each([
    ["control_log_recovery_unavailable", "Command history recovery is unavailable."],
    ["control_log_recovery_invalid", "This recovery request is invalid."],
    ["control_log_recovery_missing", "The selected conversation or command history was not found."],
    ["control_log_recovery_denied", "Recovery was not authorized."],
    ["a_future_error", "The cause is unavailable."],
  ])("reports %s without blaming storage or copying arbitrary server text", async (code, expected) => {
    const recoverControlLog = vi.fn().mockRejectedValue({ code, message: "private server details" });
    render(<ControlLogRecoveryCard bridge={{ recoverControlLog } as unknown as DesktopBridge} workspaceId="workspace" session={{ id: "session", runCount: 0 }} />);
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain(expected);
    expect(alert.textContent).not.toContain("storage");
    expect(alert.textContent).not.toContain("private server details");
    expect(screen.queryByRole("button", { name: "Confirm recovery" })).toBeNull();
  });

  it("retains the frozen evidence after a conflict and confirms only a newly requested preview", async () => {
    const freshPreview = { ...preview, authority: { ...preview.authority, request: { ...preview.authority.request, operation_id: "fresh-operation" } } };
    const recoverControlLog = vi.fn()
      .mockResolvedValueOnce({ Preview: preview })
      .mockRejectedValueOnce({ code: "control_log_recovery_repreview_required" })
      .mockResolvedValueOnce({ Preview: freshPreview })
      .mockResolvedValueOnce({ Activated: { command_generation: 1 } });
    render(<ControlLogRecoveryCard bridge={{ recoverControlLog } as unknown as DesktopBridge} workspaceId="workspace" session={{ id: "session", runCount: 0 }} />);
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    fireEvent.click(await screen.findByRole("button", { name: "Confirm recovery" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Request a new preview");
    expect(screen.getByText(preview.authority.old_content_digest)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Confirm recovery" }) as HTMLButtonElement).disabled).toBe(true);
    expect(recoverControlLog.mock.calls[1][2]).toEqual({ SealAndRotate: { preview } });
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Confirm recovery" }));
    await screen.findByRole("status");
    expect(recoverControlLog.mock.calls[3][2]).toEqual({ SealAndRotate: { preview: freshPreview } });
  });

  it.each(["control_log_recovery_unavailable", "future-preview-error"])("keeps a conflicted preview invalid after %s until a new preview succeeds", async (code) => {
    const freshPreview = { ...preview, authority: { ...preview.authority, old_content_digest: "1".repeat(64), request: { ...preview.authority.request, operation_id: "replacement-operation" } } };
    const recoverControlLog = vi.fn()
      .mockResolvedValueOnce({ Preview: preview })
      .mockRejectedValueOnce({ code: "control_log_recovery_repreview_required" })
      .mockRejectedValueOnce({ code })
      .mockResolvedValueOnce({ Preview: freshPreview })
      .mockResolvedValueOnce({ Activated: { command_generation: 1 } });
    render(<ControlLogRecoveryCard bridge={{ recoverControlLog } as unknown as DesktopBridge} workspaceId="workspace" session={{ id: "session", runCount: 0 }} />);
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    fireEvent.click(await screen.findByRole("button", { name: "Confirm recovery" }));
    expect((await screen.findByRole("alert")).textContent).toContain("Request a new preview");
    expect((screen.getByRole("button", { name: "Confirm recovery" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    expect((await screen.findByRole("alert")).textContent).not.toContain("Request a new preview");
    expect(screen.getByText(preview.authority.old_content_digest)).toBeTruthy();
    const confirm = screen.getByRole("button", { name: "Confirm recovery" });
    expect((confirm as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(confirm);
    expect(recoverControlLog).toHaveBeenCalledTimes(3);
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await screen.findByText(freshPreview.authority.old_content_digest);
    expect((screen.getByRole("button", { name: "Confirm recovery" }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Confirm recovery" }));
    await screen.findByRole("status");
    expect(recoverControlLog.mock.calls[4][2]).toEqual({ SealAndRotate: { preview: freshPreview } });
  });

  it.each(["workspace", "session"] as const)("clears invalidation and all old evidence after a %s change", async (scope) => {
    const recoverControlLog = vi.fn()
      .mockResolvedValueOnce({ Preview: preview })
      .mockRejectedValueOnce({ code: "control_log_recovery_repreview_required" })
      .mockResolvedValueOnce({ Preview: preview });
    const bridge = { recoverControlLog } as unknown as DesktopBridge;
    const page = (workspaceId: string, id: string) => <ControlLogRecoveryCard bridge={bridge} workspaceId={workspaceId} session={{ id, runCount: 0 }} />;
    const { rerender } = render(page("first-workspace", "first-session"));
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    fireEvent.click(await screen.findByRole("button", { name: "Confirm recovery" }));
    await screen.findByRole("alert");
    expect((screen.getByRole("button", { name: "Confirm recovery" }) as HTMLButtonElement).disabled).toBe(true);
    const nextWorkspace = scope === "workspace" ? "second-workspace" : "first-workspace";
    const nextSession = scope === "session" ? "second-session" : "first-session";
    rerender(page(nextWorkspace, nextSession));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button", { name: "Confirm recovery" })).toBeNull();
    expect(screen.queryByText(preview.authority.old_content_digest)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    expect((await screen.findByRole("button", { name: "Confirm recovery" }) as HTMLButtonElement).disabled).toBe(false);
    expect(recoverControlLog).toHaveBeenLastCalledWith(nextWorkspace, nextSession, "Preview");
  });

  it.each(["workspace", "session"] as const)("discards a late response and old state after a %s switch", async (scope) => {
    let resolveOld!: (value: { Preview: ControlLogRecoveryPreview }) => void;
    const recoverControlLog = vi.fn()
      .mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }))
      .mockResolvedValueOnce({ Preview: preview });
    const bridge = { recoverControlLog } as unknown as DesktopBridge;
    const page = (workspaceId: string, id: string) => <ControlLogRecoveryCard bridge={bridge} workspaceId={workspaceId} session={{ id, runCount: 0 }} />;
    const { rerender } = render(page("first-workspace", "first-session"));
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    const nextWorkspace = scope === "workspace" ? "second-workspace" : "first-workspace";
    const nextSession = scope === "session" ? "second-session" : "first-session";
    rerender(page(nextWorkspace, nextSession));
    expect((screen.getByRole("button", { name: "Preview recovery" }) as HTMLButtonElement).disabled).toBe(false);
    await act(async () => resolveOld({ Preview: preview }));
    expect(screen.queryByRole("button", { name: "Confirm recovery" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await screen.findByRole("button", { name: "Confirm recovery" });
    expect(recoverControlLog).toHaveBeenLastCalledWith(nextWorkspace, nextSession, "Preview");
  });
});
