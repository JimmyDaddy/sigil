import type { RecentWorkspaceSummary, WorkspaceSummary } from "../../types";

type WorkspaceIdentity = Pick<WorkspaceSummary | RecentWorkspaceSummary, "id" | "displayName" | "locationLabel">;

export function workspaceDisplayLabel(workspace: WorkspaceIdentity, candidates: readonly WorkspaceIdentity[]): string {
  const sameName = new Set(candidates.filter((candidate) => candidate.displayName === workspace.displayName).map((candidate) => candidate.id));
  return sameName.size > 1 && workspace.locationLabel !== undefined
    ? `${workspace.displayName} · ${workspace.locationLabel}`
    : workspace.displayName;
}
