import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  CatalogPage,
  CatalogRequest,
  DesktopBootstrap,
  SessionOpenInput,
  SessionSummary,
  RunStreamStatus,
  RunSummary,
  TimelineEvent,
  WorkspaceSelection,
  WorkspaceSummary,
  ApprovalDecisionSummary,
  TimelineApproval,
  VerificationRerunBinding,
  VerificationSummary,
} from "./types";

export interface DesktopBridge {
  bootstrap(): Promise<DesktopBootstrap>;
  pickWorkspace(): Promise<WorkspaceSelection>;
  openRecentWorkspace(recentId: string): Promise<WorkspaceSummary>;
  closeWorkspace(workspaceId: string): Promise<WorkspaceSummary[]>;
  catalog(workspaceId: string, request: CatalogRequest): Promise<CatalogPage>;
  createSession(workspaceId: string, label?: string): Promise<SessionSummary>;
  openSession(
    workspaceId: string,
    input: SessionOpenInput,
  ): Promise<SessionSummary>;
  startRun(workspaceId: string, sessionId: string, prompt: string): Promise<RunSummary>;
  cancelRun(workspaceId: string, sessionId: string, runId: string): Promise<RunSummary>;
  resolveApproval(
    workspaceId: string,
    sessionId: string,
    runId: string,
    approval: TimelineApproval,
    approve: boolean,
  ): Promise<ApprovalDecisionSummary>;
  verification(workspaceId: string, sessionId: string): Promise<VerificationSummary>;
  rerunVerification(
    workspaceId: string,
    sessionId: string,
    request: VerificationRerunBinding,
  ): Promise<VerificationSummary>;
  subscribeRunEvents(listener: (event: TimelineEvent) => void): Promise<() => void>;
  subscribeRunStreamStatus(listener: (status: RunStreamStatus) => void): Promise<() => void>;
}

export const desktopBridge: DesktopBridge = {
  bootstrap: () => invoke<DesktopBootstrap>("desktop_bootstrap"),
  pickWorkspace: () =>
    invoke<WorkspaceSelection>("desktop_pick_workspace"),
  openRecentWorkspace: (recentId) =>
    invoke<WorkspaceSummary>("desktop_open_recent_workspace", { recentId }),
  closeWorkspace: (workspaceId) =>
    invoke<WorkspaceSummary[]>("desktop_close_workspace", { workspaceId }),
  catalog: (workspaceId, request) =>
    invoke<CatalogPage>("desktop_catalog", { workspaceId, request }),
  createSession: (workspaceId, label) =>
    invoke<SessionSummary>("desktop_create_session", {
      workspaceId,
      input: { label },
    }),
  openSession: (workspaceId, input) =>
    invoke<SessionSummary>("desktop_open_session", { workspaceId, input }),
  startRun: (workspaceId, sessionId, prompt) =>
    invoke<RunSummary>("desktop_start_run", {
      workspaceId,
      input: { sessionId, prompt },
    }),
  cancelRun: (workspaceId, sessionId, runId) =>
    invoke<RunSummary>("desktop_cancel_run", {
      workspaceId,
      input: { sessionId, runId },
    }),
  resolveApproval: (workspaceId, sessionId, runId, approval, approve) =>
    invoke<ApprovalDecisionSummary>("desktop_resolve_approval", {
      workspaceId,
      input: {
        sessionId,
        runId,
        callId: approval.callId,
        approvalRequestId: approval.approvalRequestId,
        toolCallHash: approval.toolCallHash,
        policyVersion: approval.policyVersion,
        expiresAtMs: approval.expiresAtMs,
        approve,
      },
    }),
  verification: (workspaceId, sessionId) =>
    invoke<VerificationSummary>("desktop_verification", { workspaceId, sessionId }),
  rerunVerification: (workspaceId, sessionId, request) =>
    invoke<VerificationSummary>("desktop_rerun_verification", {
      workspaceId,
      input: { sessionId, request },
    }),
  subscribeRunEvents: async (listener) =>
    listen<TimelineEvent>("sigil-run-event", (event) => listener(event.payload)),
  subscribeRunStreamStatus: async (listener) =>
    listen<RunStreamStatus>("sigil-run-stream-status", (event) => listener(event.payload)),
};
