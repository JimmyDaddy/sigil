import { useCallback, useRef } from "react";

export const settingsCategories = ["connections", "preferences", "extensions", "support"] as const;
export type SettingsCategory = typeof settingsCategories[number];

export interface SettingsNavigationState {
  readonly category: SettingsCategory;
  readonly scrollTop: number;
  readonly focusKey?: string;
}

const MAX_REMEMBERED_WORKSPACES = 16;

/** Keeps only navigation context in memory while Settings is unmounted. */
export function useSettingsNavigation(workspaceId?: string) {
  const remembered = useRef(new Map<string | undefined, SettingsNavigationState>());
  const remember = useCallback((state: SettingsNavigationState) => {
    const entries = remembered.current;
    entries.delete(workspaceId);
    entries.set(workspaceId, { ...state, scrollTop: Math.max(0, state.scrollTop) });
    if (entries.size > MAX_REMEMBERED_WORKSPACES) entries.delete(entries.keys().next().value);
  }, [workspaceId]);

  return { value: remembered.current.get(workspaceId), remember } as const;
}
