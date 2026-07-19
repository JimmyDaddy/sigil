import type { CatalogEntry, CatalogPage, CatalogSourceState } from "./types";

export type HistoryState =
  | "idle"
  | "loading"
  | "ready"
  | "loading_more"
  | "stale"
  | "error";

export function HistoryContent({
  state,
  page,
  onRetry,
  onLoadMore,
  onOpen,
}: {
  state: HistoryState;
  page: CatalogPage;
  onRetry: () => void;
  onLoadMore: () => void;
  onOpen: (entry: CatalogEntry) => void;
}) {
  if (state === "loading") {
    return <div className="history-notice busy">Rebuilding local history index…</div>;
  }
  if (state === "error" || state === "stale") {
    return (
      <div className="history-notice error" role="alert">
        <strong>{state === "stale" ? "History changed while paging." : "History is unavailable."}</strong>
        <span>{state === "stale" ? "Restart from the first page to use a consistent index generation." : "The durable records are unchanged. Retry the local projection."}</span>
        <button className="quiet-button" type="button" onClick={onRetry}>Refresh history</button>
      </div>
    );
  }
  if (state === "idle") return null;

  const hasWarnings = page.degradedSourceCount > 0 || page.identityConflictCount > 0 || page.truncatedSourceCount > 0;
  return (
    <div className="history-results">
      <div className="history-meta">
        <span>{page.entries.length} conversations</span>
        <small>Generation {page.generation} · refreshed {formatTime(page.reconciledAtUnixMs)}</small>
      </div>
      {hasWarnings ? (
        <div className="history-warning" role="status">
          Some sources need attention: {page.degradedSourceCount} degraded, {page.identityConflictCount} identity conflicts, {page.truncatedSourceCount} scan-limited.
        </div>
      ) : null}
      {page.entries.length === 0 ? (
        <div className="history-empty">
          <span aria-hidden="true">◇</span>
          <strong>No matching conversation.</strong>
          <p>Start a new conversation or adjust the filters.</p>
        </div>
      ) : (
        <ul className="history-list">
          {page.entries.map((entry) => {
            const canOpen = entry.sourceState === "ready" && entry.sessionId !== undefined;
            return (
              <li key={`${entry.sessionRef}:${entry.sessionId ?? entry.sourceState}`}>
                <div className="history-row-copy">
                  <div className="history-row-title">
                    <strong>{entry.title ?? "Untitled conversation"}</strong>
                    {entry.pinned ? <span className="pin-badge">Pinned</span> : null}
                    <span className={`source-badge source-${entry.sourceState}`}>{sourceLabel(entry.sourceState)}</span>
                  </div>
                  <p>{entry.providerName ?? "Unknown provider"}{entry.modelName ? ` · ${entry.modelName}` : ""}</p>
                  <small>{entry.userMessageCount} prompts · {entry.assistantMessageCount} replies · {entry.toolResultCount} tool results · {formatTime(entry.sourceModifiedAtUnixMs)}</small>
                </div>
                <button className="quiet-button" type="button" disabled={!canOpen} onClick={() => onOpen(entry)}>
                  {canOpen ? "Open" : "Inspect only"}
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {page.nextCursor !== undefined ? (
        <button className="load-more" type="button" onClick={onLoadMore} disabled={state === "loading_more"}>
          {state === "loading_more" ? "Loading…" : "Load more"}
        </button>
      ) : null}
    </div>
  );
}

function sourceLabel(state: CatalogSourceState): string {
  switch (state) {
    case "ready": return "Ready";
    case "oversized": return "Oversized";
    case "scan_budget_exceeded": return "Scan limited";
    case "unsupported_legacy": return "Unsupported";
    case "invalid": return "Invalid";
  }
}

function formatTime(value: number): string {
  if (value <= 0) return "just now";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}
