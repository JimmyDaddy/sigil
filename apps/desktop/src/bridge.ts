import { invoke } from "@tauri-apps/api/core";

import type {
  DesktopBootstrap,
  WorkspaceSelection,
  WorkspaceSummary,
} from "./types";

export interface DesktopBridge {
  bootstrap(): Promise<DesktopBootstrap>;
  pickWorkspace(): Promise<WorkspaceSelection>;
  closeWorkspace(workspaceId: string): Promise<WorkspaceSummary[]>;
}

export const desktopBridge: DesktopBridge = {
  bootstrap: () => invoke<DesktopBootstrap>("desktop_bootstrap"),
  pickWorkspace: () =>
    invoke<WorkspaceSelection>("desktop_pick_workspace"),
  closeWorkspace: (workspaceId) =>
    invoke<WorkspaceSummary[]>("desktop_close_workspace", { workspaceId }),
};
