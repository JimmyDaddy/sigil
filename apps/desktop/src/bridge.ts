import { invoke } from "@tauri-apps/api/core";

import type {
  CatalogPage,
  CatalogRequest,
  DesktopBootstrap,
  SessionOpenInput,
  SessionSummary,
  WorkspaceSelection,
  WorkspaceSummary,
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
};
