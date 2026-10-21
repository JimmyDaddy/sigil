import type { DesktopBridge } from "../../bridge";
import type { CatalogEntry } from "../../types";

const LOOKUP_PAGE_SIZE = 100;
const MAX_LOOKUP_PAGES = 10;

/** Find only the identity returned by create, independently of the visible list's filters. */
export async function findCreatedSessionEntry(
  bridge: Pick<DesktopBridge, "catalog">,
  workspaceId: string,
  durableSessionId: string,
  isCurrent: () => boolean,
): Promise<CatalogEntry | undefined> {
  let cursor: string | undefined;
  const visitedCursors = new Set<string>();
  for (let pageIndex = 0; pageIndex < MAX_LOOKUP_PAGES && isCurrent(); pageIndex += 1) {
    const page = await bridge.catalog(workspaceId, { limit: LOOKUP_PAGE_SIZE, cursor });
    if (!isCurrent() || page.workspaceId !== workspaceId) return undefined;
    const entry = page.entries.find((candidate) => candidate.sessionId === durableSessionId);
    if (entry !== undefined) return entry.sourceState === "ready" ? entry : undefined;
    if (page.nextCursor === undefined || visitedCursors.has(page.nextCursor)) return undefined;
    cursor = page.nextCursor;
    visitedCursors.add(cursor);
  }
  return undefined;
}
