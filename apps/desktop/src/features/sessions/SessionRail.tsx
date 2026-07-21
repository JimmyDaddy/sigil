import type { FormEvent, RefObject } from "react";

import { ErrorCard } from "../../ErrorCard";
import { HistoryContent, type HistoryState } from "../../HistoryPanel";
import type { CatalogEntry, CatalogPage, CatalogSourceState } from "../../types";
import { useLocale } from "../../i18n";
import { Icon } from "../../ui/icons";
import { Button, Checkbox, IconButton, Popover, Select, TextField } from "../../ui/primitives";

interface SessionRailProps {
  readonly historyState: HistoryState;
  readonly catalog: CatalogPage;
  readonly selectedSessionId?: string;
  readonly navigationBusy: boolean;
  readonly openingSessionRef?: string;
  readonly sessionMessage?: string;
  readonly sessionError: boolean;
  readonly searchDraft: string;
  readonly searchInputRef?: RefObject<HTMLInputElement | null>;
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
  navigationBusy,
  openingSessionRef,
  sessionMessage,
  sessionError,
  searchDraft,
  searchInputRef,
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
  const { t } = useLocale();
  const activeFilterCount = Number(providerFilter.trim() !== "") + Number(sourceFilter !== "all") + Number(pinnedOnly);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    onSearch();
  };
  return (
    <section className="session-rail" aria-labelledby="session-rail-title">
      <header className="session-rail-header">
        <h2 id="session-rail-title">{t("conversations")}</h2>
      </header>
      <form className="session-search" onSubmit={submit}>
        <TextField
          label={t("searchConversations")}
          labelHidden
          ref={searchInputRef}
          value={searchDraft}
          onChange={(event) => onSearchDraftChange(event.target.value)}
          placeholder={t("searchConversations")}
        />
        <IconButton aria-label={t("search")} icon={<Icon name="search" />} type="submit" />
        <Popover
          className="session-filter-popover"
          label={<span className="filter-trigger-label"><Icon name="filter" />{activeFilterCount > 0 ? <span className="filter-count">{activeFilterCount}</span> : null}</span>}
          accessibleLabel={activeFilterCount === 0 ? t("filters") : t("activeFilters", { count: activeFilterCount })}
        >
          <div className="session-filter-panel">
            <TextField label={t("provider")} value={providerFilter} onChange={(event) => onProviderFilterChange(event.target.value)} placeholder={t("providerName")} />
            <Select label={t("sourceState")} value={sourceFilter} onChange={(event) => onSourceFilterChange(event.target.value as CatalogSourceState | "all")}>
              <option value="all">{t("allStates")}</option>
              <option value="ready">{t("ready")}</option>
              <option value="oversized">{t("oversized")}</option>
              <option value="scan_budget_exceeded">{t("scanLimited")}</option>
              <option value="unsupported_legacy">{t("unsupported")}</option>
              <option value="invalid">{t("invalid")}</option>
            </Select>
            <Checkbox label={t("pinnedOnly")} checked={pinnedOnly} onChange={(event) => onPinnedOnlyChange(event.target.checked)} />
            <Button type="button" variant="quiet" disabled={activeFilterCount === 0} onClick={onClearFilters}>{t("clearFilters")}</Button>
          </div>
        </Popover>
      </form>
      {activeFilterCount > 0 ? (
        <div className="active-filter-summary">
          <span>{activeFilterCount === 1 ? t("activeFilter") : t("activeFilters", { count: activeFilterCount })}</span>
          <Button type="button" variant="quiet" onClick={onClearFilters}>{t("clear")}</Button>
        </div>
      ) : null}
      {sessionMessage === undefined ? null : sessionError ? (
        <ErrorCard title={t("conversationUnavailable")} message={sessionMessage} actionLabel={t("refreshConversations")} onAction={onRetry} />
      ) : (
        <div className="session-notice" role="status">{sessionMessage}</div>
      )}
      <div className="session-rail-scroll">
        <HistoryContent
          state={historyState}
          page={catalog}
          selectedSessionId={selectedSessionId}
          navigationBusy={navigationBusy}
          openingSessionRef={openingSessionRef}
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
