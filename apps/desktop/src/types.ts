export type ConnectionState = "ready" | "exited" | "crashed";

export interface WorkspaceSummary {
  id: string;
  displayName: string;
  serverVersion: string;
  state: ConnectionState;
}

export interface DesktopBootstrap {
  protocolVersion: 1;
  workspaces: WorkspaceSummary[];
  recentWorkspaces: RecentWorkspaceSummary[];
}

export interface WorkspaceSelection {
  cancelled: boolean;
  workspace?: WorkspaceSummary;
}

export interface RecentWorkspaceSummary {
  id: string;
  displayName: string;
  isOpen: boolean;
}

export type CatalogSourceState =
  | "ready"
  | "oversized"
  | "scan_budget_exceeded"
  | "unsupported_legacy"
  | "invalid";

export interface CatalogRequest {
  limit?: number;
  cursor?: string;
  query?: string;
  provider?: string;
  pinned?: boolean;
  state?: CatalogSourceState;
}

export interface CatalogEntry {
  sessionRef: string;
  sessionId?: string;
  sourceState: CatalogSourceState;
  sourceModifiedAtUnixMs: number;
  providerName?: string;
  modelName?: string;
  title?: string;
  userMessageCount: number;
  assistantMessageCount: number;
  toolResultCount: number;
  pinned: boolean;
}

export interface CatalogPage {
  workspaceId: string;
  generation: number;
  reconciledAtUnixMs: number;
  degradedSourceCount: number;
  identityConflictCount: number;
  truncatedSourceCount: number;
  entries: CatalogEntry[];
  nextCursor?: string;
}

export interface SessionSummary {
  id: string;
  label?: string;
  runCount: number;
  foregroundRunId?: string;
}

export interface SessionOpenInput {
  sessionRef: string;
  sessionId: string;
  label?: string;
}
