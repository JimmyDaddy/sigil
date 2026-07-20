import type { CatalogEntry, CatalogPage, CatalogSourceState } from "./types";
import { ErrorCard } from "./ErrorCard";
import { Button, Collapsible } from "./ui/primitives";

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
  selectedSessionId,
}: {
  state: HistoryState;
  page: CatalogPage;
  onRetry: () => void;
  onLoadMore: () => void;
  onOpen: (entry: CatalogEntry) => void;
  selectedSessionId?: string;
}) {
  if (state === "loading") {
    return <div className="history-notice busy">Loading conversations…</div>;
  }
  if (state === "error" || state === "stale") {
    return (
      <ErrorCard
        title={state === "stale" ? "History changed while paging." : "History is unavailable."}
        message={state === "stale" ? "The list changed while more items were loading. Refresh and continue." : "Your saved conversations are unchanged. Try loading the list again."}
        actionLabel="Refresh conversations"
        onAction={onRetry}
      />
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
        <Collapsible
          className="catalog-degraded-summary"
          label="Some sources need attention"
          summary={`${page.degradedSourceCount + page.identityConflictCount + page.truncatedSourceCount}`}
        >
          <ul>
            {page.degradedSourceCount > 0 ? <li>{page.degradedSourceCount} unavailable</li> : null}
            {page.identityConflictCount > 0 ? <li>{page.identityConflictCount} changed</li> : null}
            {page.truncatedSourceCount > 0 ? <li>{page.truncatedSourceCount} too large to inspect here</li> : null}
          </ul>
        </Collapsible>
      ) : null}
      {page.entries.length === 0 ? (
        <div className="history-empty">
          <strong>No matching conversation.</strong>
          <p>Start a new conversation or adjust the filters.</p>
        </div>
      ) : (
        <ul className="history-list">
          {page.entries.map((entry) => {
            const canOpen = entry.sourceState === "ready" && entry.sessionId !== undefined;
            const content = (
              <>
                <span className="session-row-title">
                  <strong>{entry.title ?? "Untitled conversation"}</strong>
                  {entry.pinned ? <span className="pin-badge">Pinned</span> : null}
                  {entry.sourceState === "ready" ? null : <span className={`source-badge source-${entry.sourceState}`}>{sourceLabel(entry.sourceState)}</span>}
                </span>
                <span className="session-row-context">
                  {entry.providerName ?? "Provider unavailable"}{entry.modelName ? ` · ${entry.modelName}` : ""}
                </span>
                <small>{entry.userMessageCount} prompts · {entry.assistantMessageCount} replies · {formatTime(entry.sourceModifiedAtUnixMs)}</small>
              </>
            );
            return (
              <li key={`${entry.sessionRef}:${entry.sessionId ?? entry.sourceState}`}>
                {canOpen ? (
                  <Button
                    className="session-row"
                    type="button"
                    variant="quiet"
                    aria-current={entry.sessionId === selectedSessionId ? "page" : undefined}
                    onClick={() => onOpen(entry)}
                  >
                    {content}
                  </Button>
                ) : (
                  <div className="session-row session-row-unavailable" aria-disabled="true">{content}</div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {page.nextCursor !== undefined ? (
        <Button className="load-more" type="button" onClick={onLoadMore} disabled={state === "loading_more"}>
          {state === "loading_more" ? "Loading…" : "Load more"}
        </Button>
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
