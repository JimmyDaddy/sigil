import type { FormEvent } from "react";

import { ErrorCard } from "../../ErrorCard";
import { HistoryContent, type HistoryState } from "../../HistoryPanel";
import type { CatalogEntry, CatalogPage, CatalogSourceState } from "../../types";
import { Icon } from "../../ui/icons";
import { Button, Checkbox, IconButton, Popover, Select, TextField } from "../../ui/primitives";

interface SessionRailProps {
  readonly historyState: HistoryState;
  readonly catalog: CatalogPage;
  readonly selectedSessionId?: string;
  readonly sessionMessage?: string;
  readonly sessionError: boolean;
  readonly searchDraft: string;
  readonly providerFilter: string;
  readonly sourceFilter: CatalogSourceState | "all";
  readonly pinnedOnly: boolean;
  readonly onSearchDraftChange: (value: string) => void;
  readonly onSearch: () => void;
  readonly onProviderFilterChange: (value: string) => void;
  readonly onSourceFilterChange: (value: CatalogSourceState | "all") => void;
  readonly onPinnedOnlyChange: (value: boolean) => void;
  readonly onClearFilters: () => void;
  readonly onRetry: () => void;
  readonly onLoadMore: () => void;
  readonly onOpen: (entry: CatalogEntry) => void;
  readonly onRename: (entry: CatalogEntry) => void;
  readonly onDelete: (entry: CatalogEntry) => void;
  readonly onQuarantine: (entry: CatalogEntry) => void;
}

export function SessionRail({
  historyState,
  catalog,
  selectedSessionId,
  sessionMessage,
  sessionError,
  searchDraft,
  providerFilter,
  sourceFilter,
  pinnedOnly,
  onSearchDraftChange,
  onSearch,
  onProviderFilterChange,
  onSourceFilterChange,
  onPinnedOnlyChange,
  onClearFilters,
  onRetry,
  onLoadMore,
  onOpen,
  onRename,
  onDelete,
  onQuarantine,
}: SessionRailProps) {
  const activeFilterCount = Number(providerFilter.trim() !== "") + Number(sourceFilter !== "all") + Number(pinnedOnly);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    onSearch();
  };
  return (
    <section className="session-rail" aria-labelledby="session-rail-title">
      <header className="session-rail-header">
        <h2 id="session-rail-title">Conversations</h2>
      </header>
      <form className="session-search" onSubmit={submit}>
        <TextField
          label="Search conversations"
          labelHidden
          value={searchDraft}
          onChange={(event) => onSearchDraftChange(event.target.value)}
          placeholder="Search conversations"
        />
        <IconButton aria-label="Search" icon={<Icon name="search" />} type="submit" />
        <Popover
          className="session-filter-popover"
          label={<span className="filter-trigger-label"><Icon name="filter" />{activeFilterCount > 0 ? <span className="filter-count">{activeFilterCount}</span> : null}</span>}
          accessibleLabel={`Filters${activeFilterCount === 0 ? "" : `, ${activeFilterCount} active`}`}
        >
          <div className="session-filter-panel">
            <TextField label="Provider" value={providerFilter} onChange={(event) => onProviderFilterChange(event.target.value)} placeholder="Provider name" />
            <Select label="Source state" value={sourceFilter} onChange={(event) => onSourceFilterChange(event.target.value as CatalogSourceState | "all")}>
              <option value="all">All states</option>
              <option value="ready">Ready</option>
              <option value="oversized">Oversized</option>
              <option value="scan_budget_exceeded">Scan limited</option>
              <option value="unsupported_legacy">Unsupported</option>
              <option value="invalid">Invalid</option>
            </Select>
            <Checkbox label="Pinned only" checked={pinnedOnly} onChange={(event) => onPinnedOnlyChange(event.target.checked)} />
            <Button type="button" variant="quiet" disabled={activeFilterCount === 0} onClick={onClearFilters}>Clear filters</Button>
          </div>
        </Popover>
      </form>
      {activeFilterCount > 0 ? (
        <div className="active-filter-summary">
          <span>{activeFilterCount} active filter{activeFilterCount === 1 ? "" : "s"}</span>
          <Button type="button" variant="quiet" onClick={onClearFilters}>Clear</Button>
        </div>
      ) : null}
      {sessionMessage === undefined ? null : sessionError ? (
        <ErrorCard title="Conversation unavailable" message={sessionMessage} actionLabel="Refresh conversations" onAction={onRetry} />
      ) : (
        <div className="session-notice" role="status">{sessionMessage}</div>
      )}
      <div className="session-rail-scroll">
        <HistoryContent
          state={historyState}
          page={catalog}
          selectedSessionId={selectedSessionId}
          onRetry={onRetry}
          onLoadMore={onLoadMore}
          onOpen={onOpen}
          onRename={onRename}
          onDelete={onDelete}
          onQuarantine={onQuarantine}
        />
      </div>
    </section>
  );
}
