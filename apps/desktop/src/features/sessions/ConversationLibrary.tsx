import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type {
  CatalogEntry,
  CatalogPage,
  CatalogSourceDiagnostic,
  CatalogSourceState,
  SessionCatalogBatchAction,
  SessionCatalogBatchItem,
  SessionCatalogBatchPlan,
  SessionCatalogBatchReceipt,
} from "../../types";
import { ErrorCard } from "../../ErrorCard";
import { LoadingState, PaginationControl, useNotifications } from "../../ui/feedback";
import { Icon } from "../../ui/icons";
import { Button, Checkbox, Collapsible, Dialog, Popover, Select, TextField } from "../../ui/primitives";
import { ApplicationPage } from "../navigation/ApplicationPage";

const EMPTY_PAGE: CatalogPage = {
  workspaceId: "",
  generation: 0,
  reconciledAtUnixMs: 0,
  degradedSourceCount: 0,
  identityConflictCount: 0,
  truncatedSourceCount: 0,
  entries: [],
};

export function ConversationLibrary({
  bridge,
  workspaceId,
  onBack,
  onCatalogChanged,
  onOpen,
}: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly onBack: () => void;
  readonly onCatalogChanged: () => Promise<void>;
  readonly onOpen: (entry: CatalogEntry) => void;
}) {
  const { locale, t } = useLocale();
  const { notify } = useNotifications();
  const [page, setPage] = useState<CatalogPage>(EMPTY_PAGE);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState(false);
  const [queryDraft, setQueryDraft] = useState("");
  const [query, setQuery] = useState("");
  const [provider, setProvider] = useState("");
  const [sourceState, setSourceState] = useState<CatalogSourceState | "all">("all");
  const [pinnedOnly, setPinnedOnly] = useState(false);
  const [selectedRefs, setSelectedRefs] = useState<Set<string>>(() => new Set());
  const [planning, setPlanning] = useState(false);
  const [executing, setExecuting] = useState(false);
  const [plan, setPlan] = useState<SessionCatalogBatchPlan>();
  const [plannedItems, setPlannedItems] = useState<SessionCatalogBatchItem[]>([]);
  const [plannedEntries, setPlannedEntries] = useState<ReadonlyMap<string, CatalogEntry>>(new Map());
  const [receipt, setReceipt] = useState<SessionCatalogBatchReceipt>();
  const cancelPreviewRef = useRef<HTMLButtonElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const activeFilterCount = Number(query.trim() !== "") + Number(provider.trim() !== "")
    + Number(sourceState !== "all") + Number(pinnedOnly);
  const clearFilters = () => {
    setQueryDraft("");
    setQuery("");
    setProvider("");
    setSourceState("all");
    setPinnedOnly(false);
  };

  const request = useCallback((cursor?: string) => ({
    limit: 100,
    cursor,
    query: query.trim() || undefined,
    provider: provider.trim() || undefined,
    pinned: pinnedOnly || undefined,
    state: sourceState === "all" ? undefined : sourceState,
  }), [pinnedOnly, provider, query, sourceState]);

  const load = useCallback(async (cursor?: string) => {
    const append = cursor !== undefined;
    if (append) setLoadingMore(true);
    else setLoading(true);
    try {
      const next = await bridge.catalog(workspaceId, request(cursor));
      setPage((current) => append
        ? { ...next, entries: uniqueEntries([...current.entries, ...next.entries]) }
        : next);
      if (!append) setSelectedRefs(new Set());
      setError(false);
    } catch (cause) {
      if (append && errorCode(cause) === "catalog_stale") {
        try {
          const next = await bridge.catalog(workspaceId, request());
          setPage(next);
          setSelectedRefs(new Set());
          setError(false);
          return;
        } catch {
          // Fall through to the stable inline error.
        }
      }
      setError(true);
    } finally {
      setLoading(false);
      setLoadingMore(false);
    }
  }, [bridge, request, workspaceId]);

  useEffect(() => {
    void load();
  }, [load]);

  const selectedEntries = useMemo(
    () => page.entries.filter((entry) => selectedRefs.has(entry.sessionRef)),
    [page.entries, selectedRefs],
  );
  const selectedReady = selectedEntries.filter(
    (entry): entry is CatalogEntry & { sessionId: string } =>
      entry.sourceState === "ready" && entry.sessionId !== undefined,
  );
  const selectedUnavailable = selectedEntries.filter((entry) => entry.sourceState !== "ready");
  const allLoadedSelected = page.entries.length > 0 && selectedRefs.size === page.entries.length;

  const toggleAllLoaded = (checked: boolean) => {
    setSelectedRefs(checked ? new Set(page.entries.map((entry) => entry.sessionRef)) : new Set());
  };
  const toggleRow = (entry: CatalogEntry, checked: boolean) => {
    setSelectedRefs((current) => {
      const next = new Set(current);
      if (checked) next.add(entry.sessionRef);
      else next.delete(entry.sessionRef);
      return next;
    });
  };

  const prepare = async (action: SessionCatalogBatchAction, entries: CatalogEntry[]) => {
    const items = entries.map((entry) => batchItem(action, entry));
    const entrySnapshot = new Map(entries.map((entry) => [entry.sessionRef, { ...entry }]));
    setPlanning(true);
    try {
      const next = await bridge.planSessionCatalogBatch(workspaceId, { action, items });
      setPlannedItems(items);
      setPlannedEntries(entrySnapshot);
      setPlan(next);
    } catch {
      notify({ tone: "error", message: t("batchPlanFailed") });
      await load();
    } finally {
      setPlanning(false);
    }
  };

  const execute = async () => {
    if (plan === undefined) return;
    setExecuting(true);
    try {
      const next = await bridge.executeSessionCatalogBatch(workspaceId, {
        planId: plan.planId,
        action: plan.action,
        items: plannedItems,
      });
      setReceipt(next);
      setPlan(undefined);
      setSelectedRefs(new Set());
      await Promise.all([load(), onCatalogChanged()]);
      notify({
        tone: next.failed > 0 ? "warning" : "success",
        message: `${t("batchCompleted", { count: next.completed })} · ${t("batchFailed", { count: next.failed })}`,
      });
    } catch {
      setPlan(undefined);
      notify({ tone: "error", message: t("batchExecuteFailed") });
      await load();
    } finally {
      setExecuting(false);
    }
  };

  return (
    <ApplicationPage
      className="conversation-library-page"
      eyebrow={t("conversations")}
      title={t("conversationLibrary")}
      navigation={{ label: t("backToConversation"), onBack }}
      aside={<span className="application-page-meta">{t("loadedConversationCount", { count: page.entries.length })}</span>}
    >

      <div className="library-command-surface">
        <form className="library-filter-bar" onSubmit={(event) => { event.preventDefault(); setQuery(queryDraft); }}>
          <TextField
            ref={searchRef}
            label={t("searchConversations")}
            labelHidden
            placeholder={t("searchConversations")}
            value={queryDraft}
            onChange={(event) => {
              setQueryDraft(event.currentTarget.value);
              if (event.currentTarget.value === "") setQuery("");
            }}
          />
          <Button type="submit" leadingIcon={<Icon name="search" />}>{t("search")}</Button>
          <Popover className="library-filter-popover"
            accessibleLabel={activeFilterCount === 0 ? t("filters") : activeFilterCount === 1 ? t("activeFilter") : t("activeFilters", { count: activeFilterCount })}
            label={<span className="library-filter-trigger"><Icon name="filter" />{t("filters")}{activeFilterCount === 0 ? null : <span>{activeFilterCount}</span>}</span>}
          >
            <div className="library-filter-panel">
              <TextField label={t("provider")} placeholder={t("provider")}
                value={provider} onChange={(event) => setProvider(event.currentTarget.value)} />
              <Select label={t("sourceState")} value={sourceState}
                onChange={(event) => setSourceState(event.currentTarget.value as CatalogSourceState | "all")}>
                <option value="all">{t("allStates")}</option>
                <option value="ready">{t("ready")}</option>
                <option value="invalid">{t("invalid")}</option>
                <option value="oversized">{t("oversized")}</option>
                <option value="scan_budget_exceeded">{t("scanLimited")}</option>
              </Select>
              <Checkbox label={t("pinnedOnly")} checked={pinnedOnly} onChange={(event) => setPinnedOnly(event.currentTarget.checked)} />
              <Button type="button" variant="quiet" disabled={activeFilterCount === 0} onClick={clearFilters}>{t("clearFilters")}</Button>
            </div>
          </Popover>
        </form>
        {activeFilterCount === 0 ? null : <div className="library-active-filters">
          <div className="library-filter-values" aria-label={t("appliedLibraryFilters")}>
            {query.trim() === "" ? null : <span>{t("search")}: {query.trim()}</span>}
            {provider.trim() === "" ? null : <span>{t("provider")}: {provider.trim()}</span>}
            {sourceState === "all" ? null : <span>{stateLabel(sourceState, t)}</span>}
            {pinnedOnly ? <span>{t("pinnedOnly")}</span> : null}
          </div>
          {!loading && !error && page.entries.length === 0 ? null : <Button type="button" variant="quiet" onClick={clearFilters}>{t("clearFilters")}</Button>}
        </div>}

        {selectedEntries.length === 0 ? null : <div className="library-selection-bar" aria-live="polite">
          <span>{t("selectedConversationCount", { count: selectedEntries.length })}</span>
          <div className="library-batch-actions">
            <Button
              type="button"
              variant="danger"
              disabled={selectedReady.length === 0 || planning}
              onClick={() => void prepare("delete_sessions", selectedReady)}
            >{t("deleteSelectedReady", { count: selectedReady.length })}</Button>
            <Button
              type="button"
              disabled={selectedUnavailable.length === 0 || planning}
              onClick={() => void prepare("quarantine_invalid_sources", selectedUnavailable)}
            >{t("quarantineSelectedInvalid", { count: selectedUnavailable.length })}</Button>
            <Button
              type="button"
              variant="danger"
              disabled={selectedUnavailable.length === 0 || planning}
              onClick={() => void prepare("delete_invalid_sources", selectedUnavailable)}
            >{t("deleteSelectedInvalid", { count: selectedUnavailable.length })}</Button>
          </div>
        </div>}
      </div>

      {receipt === undefined ? null : <BatchReceipt receipt={receipt} />}

      <div className="library-table-frame">
        {loading ? <LoadingState label={t("loadingConversations")} /> : error ? (
          <ErrorCard title={t("historyUnavailable")} message={t("historyUnavailableDetail")} actionLabel={t("refreshConversations")} onAction={() => void load()} />
        ) : page.entries.length === 0 ? (
          <div className="library-empty">
            <p>{t(activeFilterCount > 0 ? "noLibraryMatch" : "noLibraryConversations")}</p>
            {activeFilterCount === 0 ? null : <Button type="button" variant="secondary" onClick={() => {
              clearFilters();
              searchRef.current?.focus();
            }}>{t("clearFilters")}</Button>}
          </div>
        ) : (
          <table className="library-table">
            <thead>
              <tr>
                <th scope="col"><Checkbox className="library-row-check"
                  label={t("selectLoadedConversations")} checked={allLoadedSelected}
                  onChange={(event) => toggleAllLoaded(event.currentTarget.checked)} /></th>
                <th scope="col">{t("conversationTitle")}</th>
                <th scope="col">{t("conversationState")}</th>
                <th scope="col">{t("provider")}</th>
                <th scope="col">{t("conversationActivity")}</th>
                <th scope="col">{t("lastUpdated")}</th>
              </tr>
            </thead>
            <tbody>
              {page.entries.map((entry) => {
                const title = entry.title ?? t("untitledConversation");
                return (
                  <tr key={`${entry.sessionRef}:${entry.sessionId ?? entry.sourceModifiedAtUnixMs}`} data-selected={selectedRefs.has(entry.sessionRef) || undefined}>
                    <td>
                      <Checkbox
                        className="library-row-check"
                        label={`${t("selectConversationRow")}: ${title}`}
                        checked={selectedRefs.has(entry.sessionRef)}
                        onChange={(event) => toggleRow(entry, event.currentTarget.checked)}
                      />
                    </td>
                    <th scope="row">
                      {entry.sourceState === "ready" ? (
                        <Button className="library-title-button" variant="quiet" type="button" onClick={() => onOpen(entry)}>{title}</Button>
                      ) : <span>{title}</span>}
                      {entry.modelName === undefined ? null : <small>{entry.modelName}</small>}
                      <small className="library-compact-meta">
                        {entry.providerName === undefined ? null : <span>{entry.providerName}</span>}
                        <span>{t("conversationMessageCount", { count: entry.userMessageCount + entry.assistantMessageCount })}
                          {" · "}{t("conversationToolResultCount", { count: entry.toolResultCount })}</span>
                        <span>{formatDate(entry.sourceModifiedAtUnixMs, locale)}</span>
                      </small>
                    </th>
                    <td>
                      <span className={`library-state state-${entry.sourceState}`}>{stateLabel(entry.sourceState, t)}</span>
                      {entry.sourceDiagnostic === undefined ? null : (
                        <small className="library-source-diagnostic">{diagnosticLabel(entry.sourceDiagnostic, t)}</small>
                      )}
                    </td>
                    <td>{entry.providerName ?? "—"}</td>
                    <td>{t("conversationMessageCount", { count: entry.userMessageCount + entry.assistantMessageCount })}
                      {" · "}{t("conversationToolResultCount", { count: entry.toolResultCount })}</td>
                    <td>{formatDate(entry.sourceModifiedAtUnixMs, locale)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {page.nextCursor === undefined ? null : (
          <PaginationControl
            className="library-pagination"
            label={t("loadMore")}
            loadingLabel={t("loading")}
            loading={loadingMore}
            onLoadMore={() => void load(page.nextCursor)}
          />
        )}
      </div>

      <Dialog
        open={plan !== undefined}
        destructive
        initialFocusRef={cancelPreviewRef}
        title={plan === undefined ? t("batchPreview") : batchActionTitle(plan.action, plan.total, t)}
        description={plan === undefined ? undefined : batchActionImpact(plan.action, t)}
        onOpenChange={(open) => { if (!open && !executing) setPlan(undefined); }}
      >
        {plan === undefined ? null : (
          <div className="batch-preview">
            <div className="batch-preview-summary">
              <strong>{t("batchExecutable", { count: plan.executable })}</strong>
              <span>{t("batchBlocked", { count: plan.blocked })}</span>
            </div>
            <ul className="batch-preview-list" aria-label={t("batchTargets")}>
              {plan.items.map((item, index) => {
                const entry = plannedEntries.get(item.sessionRef);
                const title = entry?.title?.trim() || t("untitledConversation");
                const showIdentity = !entry?.title?.trim() || [...plannedEntries.values()].filter(
                  (candidate) => candidate.title?.trim() === entry.title?.trim(),
                ).length > 1;
                return <li key={`${item.sessionRef}:${index}`}>
                  <div className="batch-target-copy">
                    <strong>{title}</strong>
                    <small>{entry?.modelName ?? t("batchModelUnknown")} · {formatDate(entry?.sourceModifiedAtUnixMs ?? 0, locale)}</small>
                    {showIdentity ? <code>{item.sessionRef}</code> : <Collapsible label={t("batchTargetIdentity")}>
                      <code>{item.sessionRef}</code>
                    </Collapsible>}
                  </div>
                  <span>{item.status === "blocked" ? reasonLabel(item.reason, t) : t("batchTargetExecutable")}</span>
                </li>;
              })}
            </ul>
            <p className="destructive-explanation">{t("batchBestEffort")}</p>
            <div className="confirmation-actions">
              <Button ref={cancelPreviewRef} type="button" disabled={executing} onClick={() => setPlan(undefined)}>{t("cancel")}</Button>
              <Button type="button" variant={plan.action === "quarantine_invalid_sources" ? "primary" : "danger"}
                busy={executing} disabled={plan.executable === 0} onClick={() => void execute()}>
                {batchActionConfirm(plan.action, plan.executable, t)}
              </Button>
            </div>
          </div>
        )}
      </Dialog>
    </ApplicationPage>
  );
}

function BatchReceipt({ receipt }: { readonly receipt: SessionCatalogBatchReceipt }) {
  const { t } = useLocale();
  return (
    <section className="batch-receipt" aria-labelledby="batch-receipt-title">
      <div>
        <h2 id="batch-receipt-title">{t("batchReceipt")}</h2>
        <p>{t("batchCompleted", { count: receipt.completed })} · {t("batchFailed", { count: receipt.failed })} · {t("batchSkipped", { count: receipt.skipped })}</p>
      </div>
      <Collapsible label={t("batchReceiptDetails")}>
        <ul>
          {receipt.items.map((item, index) => (
            <li key={`${item.sessionRef}:${index}`}><code>{item.sessionRef}</code>
              <span>{batchOutcomeLabel(item.outcome, t)}{item.reason === undefined ? "" : ` · ${reasonLabel(item.reason, t)}`}</span>
            </li>
          ))}
        </ul>
      </Collapsible>
    </section>
  );
}

function batchActionTitle(action: SessionCatalogBatchAction, count: number, t: ReturnType<typeof useLocale>["t"]): string {
  switch (action) {
    case "delete_sessions": return t("batchDeleteSessionsTitle", { count });
    case "quarantine_invalid_sources": return t("batchQuarantineSourcesTitle", { count });
    case "delete_invalid_sources": return t("batchDeleteSourcesTitle", { count });
  }
}

function batchActionImpact(action: SessionCatalogBatchAction, t: ReturnType<typeof useLocale>["t"]): string {
  switch (action) {
    case "delete_sessions": return t("deleteConversationDetail");
    case "quarantine_invalid_sources": return t("quarantineConversationDetail");
    case "delete_invalid_sources": return t("deleteInvalidSourceDetail");
  }
}

function batchActionConfirm(action: SessionCatalogBatchAction, count: number, t: ReturnType<typeof useLocale>["t"]): string {
  switch (action) {
    case "delete_sessions": return t("batchDeleteSessionsConfirm", { count });
    case "quarantine_invalid_sources": return t("batchQuarantineSourcesConfirm", { count });
    case "delete_invalid_sources": return t("batchDeleteSourcesConfirm", { count });
  }
}

function batchOutcomeLabel(outcome: SessionCatalogBatchReceipt["items"][number]["outcome"], t: ReturnType<typeof useLocale>["t"]): string {
  switch (outcome) {
    case "completed": return t("batchOutcomeCompleted");
    case "failed": return t("batchOutcomeFailed");
    case "skipped": return t("batchOutcomeSkipped");
  }
}

function batchItem(action: SessionCatalogBatchAction, entry: CatalogEntry): SessionCatalogBatchItem {
  if (action === "delete_sessions") {
    return { sessionRef: entry.sessionRef, sessionId: entry.sessionId };
  }
  return {
    sessionRef: entry.sessionRef,
    sourceBytes: entry.sourceBytes,
    sourceModifiedAtUnixMs: entry.sourceModifiedAtUnixMs,
  };
}

function uniqueEntries(entries: CatalogEntry[]): CatalogEntry[] {
  const seen = new Set<string>();
  return entries.filter((entry) => {
    const key = `${entry.sessionRef}:${entry.sessionId ?? entry.sourceModifiedAtUnixMs}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function stateLabel(state: CatalogSourceState, t: ReturnType<typeof useLocale>["t"]): string {
  switch (state) {
    case "ready": return t("ready");
    case "invalid": return t("invalid");
    case "oversized": return t("oversized");
    case "scan_budget_exceeded": return t("scanLimited");
  }
}

function reasonLabel(reason: string | undefined, t: ReturnType<typeof useLocale>["t"]): string {
  switch (reason) {
    case "pinned": return t("batchReasonPinned");
    case "active": return t("batchReasonActive");
    case "identity_changed": return t("batchReasonIdentityChanged");
    case "not_ready": return t("batchReasonNotReady");
    case "not_found": return t("batchReasonNotFound");
    case "duplicate": return t("batchReasonDuplicate");
    case "invalid_request": return t("batchReasonInvalidRequest");
    case "writer_busy": return t("batchReasonWriterBusy");
    default: return t("batchReasonUnavailable");
  }
}

function diagnosticLabel(
  diagnostic: CatalogSourceDiagnostic,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (diagnostic) {
    case "unsafe_source": return t("sourceDiagnosticUnsafe");
    case "invalid_event_stream": return t("sourceDiagnosticEventStream");
    case "invalid_projection": return t("sourceDiagnosticProjection");
    case "missing_session_identity": return t("sourceDiagnosticMissingIdentity");
  }
}

function formatDate(value: number, locale: string): string {
  if (value <= 0) return "—";
  return new Intl.DateTimeFormat(locale, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(new Date(value));
}

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}
