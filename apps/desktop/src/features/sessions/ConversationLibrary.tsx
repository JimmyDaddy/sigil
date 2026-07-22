import { useCallback, useEffect, useMemo, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type {
  CatalogEntry,
  CatalogPage,
  CatalogSourceState,
  SessionCatalogBatchAction,
  SessionCatalogBatchItem,
  SessionCatalogBatchPlan,
  SessionCatalogBatchReceipt,
} from "../../types";
import { ErrorCard } from "../../ErrorCard";
import { LoadingState, PaginationControl, useNotifications } from "../../ui/feedback";
import { Icon } from "../../ui/icons";
import { Button, Checkbox, Dialog, Select, TextField } from "../../ui/primitives";
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
  onOpen,
}: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly onBack: () => void;
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
  const [receipt, setReceipt] = useState<SessionCatalogBatchReceipt>();

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
  const selectedInvalid = selectedEntries.filter((entry) => entry.sourceState === "invalid");
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
    setPlanning(true);
    try {
      const next = await bridge.planSessionCatalogBatch(workspaceId, { action, items });
      setPlannedItems(items);
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
      await load();
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
      detail={t("conversationLibraryDetail")}
      navigation={{ label: t("backToConversation"), onBack }}
      aside={<span className="application-page-meta">{t("loadedConversationCount", { count: page.entries.length })}</span>}
    >

      <div className="library-command-surface">
        <form className="library-filter-bar" onSubmit={(event) => { event.preventDefault(); setQuery(queryDraft); }}>
          <TextField
            label={t("searchConversations")}
            labelHidden
            placeholder={t("searchConversations")}
            value={queryDraft}
            onChange={(event) => {
              setQueryDraft(event.currentTarget.value);
              if (event.currentTarget.value === "") setQuery("");
            }}
          />
          <TextField
            label={t("provider")}
            labelHidden
            placeholder={t("provider")}
            value={provider}
            onChange={(event) => setProvider(event.currentTarget.value)}
          />
          <Select
            label={t("sourceState")}
            labelHidden
            value={sourceState}
            onChange={(event) => setSourceState(event.currentTarget.value as CatalogSourceState | "all")}
          >
            <option value="all">{t("allStates")}</option>
            <option value="ready">{t("ready")}</option>
            <option value="invalid">{t("invalid")}</option>
            <option value="oversized">{t("oversized")}</option>
            <option value="scan_budget_exceeded">{t("scanLimited")}</option>
            <option value="unsupported_legacy">{t("unsupported")}</option>
          </Select>
          <Checkbox label={t("pinnedOnly")} checked={pinnedOnly} onChange={(event) => setPinnedOnly(event.currentTarget.checked)} />
          <Button type="submit" leadingIcon={<Icon name="search" />}>{t("search")}</Button>
        </form>

        <div className="library-selection-bar" aria-live="polite">
          <Checkbox
            label={t("selectLoadedConversations")}
            checked={allLoadedSelected}
            disabled={page.entries.length === 0}
            onChange={(event) => toggleAllLoaded(event.currentTarget.checked)}
          />
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
              disabled={selectedInvalid.length === 0 || planning}
              onClick={() => void prepare("quarantine_invalid_sources", selectedInvalid)}
            >{t("quarantineSelectedInvalid", { count: selectedInvalid.length })}</Button>
            <Button
              type="button"
              variant="danger"
              disabled={selectedInvalid.length === 0 || planning}
              onClick={() => void prepare("delete_invalid_sources", selectedInvalid)}
            >{t("deleteSelectedInvalid", { count: selectedInvalid.length })}</Button>
          </div>
        </div>
      </div>

      {receipt === undefined ? null : <BatchReceipt receipt={receipt} />}

      <div className="library-table-frame">
        {loading ? <LoadingState label={t("loadingConversations")} /> : error ? (
          <ErrorCard title={t("historyUnavailable")} message={t("historyUnavailableDetail")} actionLabel={t("refreshConversations")} onAction={() => void load()} />
        ) : page.entries.length === 0 ? (
          <div className="library-empty"><p>{t("noLibraryMatch")}</p></div>
        ) : (
          <table className="library-table">
            <thead>
              <tr>
                <th scope="col"><span className="sr-only">{t("selectLoadedConversations")}</span></th>
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
                      <small>{entry.modelName ?? entry.sessionRef}</small>
                    </th>
                    <td><span className={`library-state state-${entry.sourceState}`}>{stateLabel(entry.sourceState, t)}</span></td>
                    <td>{entry.providerName ?? "—"}</td>
                    <td>{entry.userMessageCount + entry.assistantMessageCount} · {entry.toolResultCount}</td>
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
        title={t("batchPreview")}
        description={t("batchPreviewDetail")}
        onOpenChange={(open) => { if (!open && !executing) setPlan(undefined); }}
      >
        {plan === undefined ? null : (
          <div className="batch-preview">
            <div className="batch-preview-summary">
              <strong>{t("batchExecutable", { count: plan.executable })}</strong>
              <span>{t("batchBlocked", { count: plan.blocked })}</span>
            </div>
            {plan.blocked === 0 ? null : (
              <ul className="batch-preview-list">
                {plan.items.filter((item) => item.status === "blocked").map((item) => (
                  <li key={item.sessionRef}><code>{item.sessionRef}</code><span>{reasonLabel(item.reason, t)}</span></li>
                ))}
              </ul>
            )}
            <p className="destructive-explanation">{t("batchBestEffort")}</p>
            <div className="confirmation-actions">
              <Button type="button" disabled={executing} onClick={() => setPlan(undefined)}>{t("cancel")}</Button>
              <Button type="button" variant="danger" busy={executing} disabled={plan.executable === 0} onClick={() => void execute()}>{t("executeBatch")}</Button>
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
      <ul>
        {receipt.items.filter((item) => item.outcome !== "completed").map((item) => (
          <li key={item.sessionRef}><code>{item.sessionRef}</code><span>{item.outcome}: {reasonLabel(item.reason, t)}</span></li>
        ))}
      </ul>
    </section>
  );
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
    case "unsupported_legacy": return t("unsupported");
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
    default: return t("batchReasonUnavailable");
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
