import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { AppearanceSnapshot, ThemePreference } from "./appearance/contract";

import type {
  CatalogPage,
  CatalogRequest,
  DesktopBootstrap,
  SessionOpenInput,
  SessionSummary,
  RunStreamStatus,
  RunAttachment,
  RunSummary,
  TimelineEvent,
  WorkspaceSelection,
  WorkspaceSummary,
  ApprovalDecisionSummary,
  TimelineApproval,
  TranscriptPage,
  TranscriptRequest,
  VerificationRerunBinding,
  VerificationSummary,
} from "./types";

export interface DesktopBridge {
  bootstrap(): Promise<DesktopBootstrap>;
  setAppearance(preference: ThemePreference): Promise<AppearanceSnapshot>;
  pickWorkspace(): Promise<WorkspaceSelection>;
  openRecentWorkspace(recentId: string): Promise<WorkspaceSummary>;
  closeWorkspace(workspaceId: string, confirmActiveRuns?: boolean): Promise<WorkspaceSummary[]>;
  catalog(workspaceId: string, request: CatalogRequest): Promise<CatalogPage>;
  createSession(workspaceId: string, label?: string): Promise<SessionSummary>;
  openSession(
    workspaceId: string,
    input: SessionOpenInput,
  ): Promise<SessionSummary>;
  transcript(
    workspaceId: string,
    sessionId: string,
    request: TranscriptRequest,
  ): Promise<TranscriptPage>;
  startRun(workspaceId: string, sessionId: string, prompt: string): Promise<RunSummary>;
  attachRun(workspaceId: string, sessionId: string, runId: string): Promise<RunAttachment>;
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
  subscribeAppearance(listener: (snapshot: AppearanceSnapshot) => void): Promise<() => void>;
}

export const desktopBridge: DesktopBridge = {
  bootstrap: () => invoke<DesktopBootstrap>("desktop_bootstrap"),
  setAppearance: (preference) =>
    invoke<AppearanceSnapshot>("desktop_set_appearance", { input: { preference } }),
  pickWorkspace: () =>
    invoke<WorkspaceSelection>("desktop_pick_workspace"),
  openRecentWorkspace: (recentId) =>
    invoke<WorkspaceSummary>("desktop_open_recent_workspace", { recentId }),
  closeWorkspace: (workspaceId, confirmActiveRuns = false) =>
    invoke<WorkspaceSummary[]>("desktop_close_workspace", {
      workspaceId,
      confirmActiveRuns,
    }),
  catalog: (workspaceId, request) =>
    invoke<CatalogPage>("desktop_catalog", { workspaceId, request }),
  createSession: (workspaceId, label) =>
    invoke<SessionSummary>("desktop_create_session", {
      workspaceId,
      input: { label },
    }),
  openSession: (workspaceId, input) =>
    invoke<SessionSummary>("desktop_open_session", { workspaceId, input }),
  transcript: (workspaceId, sessionId, request) =>
    invoke<TranscriptPage>("desktop_transcript", {
      workspaceId,
      sessionId,
      request,
    }),
  startRun: (workspaceId, sessionId, prompt) =>
    invoke<RunSummary>("desktop_start_run", {
      workspaceId,
      input: { sessionId, prompt },
    }),
  attachRun: (workspaceId, sessionId, runId) =>
    invoke<RunAttachment>("desktop_attach_run", {
      workspaceId,
      input: { sessionId, runId },
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
  subscribeAppearance: async (listener) =>
    listen<AppearanceSnapshot>("sigil-appearance-changed", (event) => listener(event.payload)),
};
