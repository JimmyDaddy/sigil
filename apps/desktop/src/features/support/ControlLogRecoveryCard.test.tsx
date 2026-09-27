import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../../bridge";
import type { ControlLogRecoveryPreview } from "../../types";
import { NotificationProvider } from "../../ui/feedback";
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
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await screen.findByRole("button", { name: "Confirm recovery" });
    rerender(page("second"));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Confirm recovery" })).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Preview recovery" }));
    await waitFor(() => expect(bridge.recoverControlLog).toHaveBeenLastCalledWith("workspace", "second", "Preview"));
  });
});
