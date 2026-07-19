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
    return <div className="history-notice busy">Loading conversations…</div>;
  }
  if (state === "error" || state === "stale") {
    return (
      <div className="history-notice error" role="alert">
        <strong>{state === "stale" ? "History changed while paging." : "History is unavailable."}</strong>
        <span>{state === "stale" ? "The list changed while more items were loading. Refresh and continue." : "Your saved conversations are unchanged. Try loading the list again."}</span>
        <button className="quiet-button" type="button" onClick={onRetry}>Refresh conversations</button>
      </div>
    );
  }
  if (state === "idle") return null;

  const hasWarnings = page.degradedSourceCount > 0 || page.identityConflictCount > 0 || page.truncatedSourceCount > 0;
  return (
    <div className="history-results">
      <div className="history-meta">
        <span>{page.entries.length} conversations</span>
        <small>Updated {formatTime(page.reconciledAtUnixMs)}</small>
      </div>
      {hasWarnings ? (
        <div className="history-warning" role="status">
          Some conversations need attention: {page.degradedSourceCount} unavailable, {page.identityConflictCount} changed, {page.truncatedSourceCount} too large to inspect here.
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
                {canOpen ? (
                  <button className="quiet-button" type="button" onClick={() => onOpen(entry)}>Open</button>
                ) : (
                  <span className="history-row-unavailable">Unavailable</span>
                )}
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
    case "unsupported_legacy": return "Unavailable";
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
