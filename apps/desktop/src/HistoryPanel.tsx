import { useEffect, useRef, type MouseEvent } from "react";

import type { CatalogEntry, CatalogPage, CatalogSourceState } from "./types";
import { useLocale } from "./i18n";
import { ErrorCard } from "./ErrorCard";
import { Icon } from "./ui/icons";
import { Button, Menu, MenuItem, Popover } from "./ui/primitives";

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
  onRename,
  onDelete,
  onQuarantine,
  selectedSessionId,
}: {
  state: HistoryState;
  page: CatalogPage;
  onRetry: () => void;
  onLoadMore: () => void;
  onOpen: (entry: CatalogEntry) => void;
  onRename: (entry: CatalogEntry) => void;
  onDelete: (entry: CatalogEntry) => void;
  onQuarantine: (entry: CatalogEntry) => void;
  selectedSessionId?: string;
}) {
  const { locale, t } = useLocale();
  const previousSessionPress = useRef<
    { readonly key: string; readonly timestamp: number } | undefined
  >(undefined);
  const suppressSessionOpen = useRef<string | undefined>(undefined);
  const pendingSessionOpen = useRef<number | undefined>(undefined);
  useEffect(() => () => {
    if (pendingSessionOpen.current !== undefined) window.clearTimeout(pendingSessionOpen.current);
  }, []);
  if (state === "loading") {
    return <div className="history-notice busy">{t("loadingConversations")}</div>;
  }
  if (state === "error" || state === "stale") {
    return (
      <ErrorCard
        title={t("historyUnavailable")}
        message={t("historyUnavailableDetail")}
        actionLabel={t("refreshConversations")}
        onAction={onRetry}
      />
    );
  }
  if (state === "idle") return null;

  const hasWarnings = page.degradedSourceCount > 0 || page.identityConflictCount > 0 || page.truncatedSourceCount > 0;
  const warningCount = page.degradedSourceCount + page.identityConflictCount + page.truncatedSourceCount;
  const groups = groupEntries(page.entries, page.reconciledAtUnixMs);
  return (
    <div className="history-results">
      <div className="history-meta">
        <span>{t("conversationCount", { count: page.entries.length })}</span>
        <span className="history-meta-actions">
          <small>{t("updated", { time: formatTime(page.reconciledAtUnixMs, locale) })}</small>
          {hasWarnings ? (
            <Popover
              className="catalog-diagnostics"
              label={<span className="catalog-diagnostics-trigger"><Icon name="warning" /><span>{warningCount}</span></span>}
              accessibleLabel={t("catalogIssues", { count: warningCount })}
            >
              <div className="catalog-diagnostics-panel">
                <strong>{t("catalogDiagnostics")}</strong>
                <p>{t("catalogAttention")}</p>
                <ul>
                  {page.degradedSourceCount > 0 ? <li><span>{t("unavailable")}</span><strong>{page.degradedSourceCount}</strong></li> : null}
                  {page.identityConflictCount > 0 ? <li><span>{t("changed")}</span><strong>{page.identityConflictCount}</strong></li> : null}
                  {page.truncatedSourceCount > 0 ? <li><span>{t("tooLarge")}</span><strong>{page.truncatedSourceCount}</strong></li> : null}
                </ul>
              </div>
            </Popover>
          ) : null}
        </span>
      </div>
      {page.entries.length === 0 ? (
        <div className="history-empty">
          <strong>{t("noMatchingConversation")}</strong>
          <p>{t("adjustFilters")}</p>
        </div>
      ) : (
        <div className="history-groups">
          {groups.map((group) => (
            <section className="history-group" key={group.id} aria-labelledby={`history-group-${group.id}`}>
              <h3 id={`history-group-${group.id}`}>{t(group.id)}</h3>
              <ul className="history-list">
                {group.entries.map((entry) => {
                  const canOpen = entry.sourceState === "ready" && entry.sessionId !== undefined;
                  const providerContext = [entry.providerName, entry.modelName].filter(Boolean).join(" · ");
                  const managementLabel = entry.title ?? entry.sessionRef;
                  const content = (
                    <>
                      <span className="session-row-title">
                        <strong>{entry.title ?? t("untitledConversation")}</strong>
                        {entry.pinned ? <span className="pin-indicator" aria-label={t("pinned")}><Icon name="pin" /></span> : null}
                        {entry.sourceState === "ready" ? null : <span className={`source-badge source-${entry.sourceState}`}>{sourceLabel(entry.sourceState, t)}</span>}
                      </span>
                      <small>
                        {formatActivityCount(entry, locale)} · {formatRelativeTime(entry.sourceModifiedAtUnixMs, page.reconciledAtUnixMs, locale)}
                      </small>
                    </>
                  );
                  return (
                    <li key={`${entry.sessionRef}:${entry.sessionId ?? entry.sourceState}`}>
                      {canOpen ? (
                        <div className="session-row-shell" onContextMenu={openContextMenu}>
                          <Button
                            className="session-row"
                            type="button"
                            variant="quiet"
                            aria-current={entry.sessionId === selectedSessionId ? "page" : undefined}
                            title={providerContext || undefined}
                            onMouseDown={(event) => {
                              if (event.button !== 0) return;
                              const entryKey = `${entry.sessionRef}:${entry.sessionId}`;
                              const previous = previousSessionPress.current;
                              const timestamp = Date.now();
                              if (previous?.key === entryKey && timestamp - previous.timestamp <= 500) {
                                previousSessionPress.current = undefined;
                                suppressSessionOpen.current = entryKey;
                                if (pendingSessionOpen.current !== undefined) {
                                  window.clearTimeout(pendingSessionOpen.current);
                                  pendingSessionOpen.current = undefined;
                                }
                                event.preventDefault();
                                onRename(entry);
                                return;
                              }
                              previousSessionPress.current = { key: entryKey, timestamp };
                            }}
                            onClick={(event) => {
                              const entryKey = `${entry.sessionRef}:${entry.sessionId}`;
                              if (suppressSessionOpen.current === entryKey || event.detail >= 2) {
                                suppressSessionOpen.current = undefined;
                                event.preventDefault();
                                return;
                              }
                              if (event.detail === 0) {
                                onOpen(entry);
                                return;
                              }
                              if (pendingSessionOpen.current !== undefined) {
                                window.clearTimeout(pendingSessionOpen.current);
                              }
                              pendingSessionOpen.current = window.setTimeout(() => {
                                pendingSessionOpen.current = undefined;
                                onOpen(entry);
                              }, 240);
                            }}
                            onDoubleClick={(event) => {
                              if (pendingSessionOpen.current !== undefined) {
                                window.clearTimeout(pendingSessionOpen.current);
                                pendingSessionOpen.current = undefined;
                              }
                              event.preventDefault();
                              onRename(entry);
                            }}
                          >
                            {content}
                          </Button>
                          <Menu
                            accessibleLabel={t("manageConversation", { name: managementLabel })}
                            label={<Icon name="more" />}
                          >
                            <MenuItem onSelect={() => onRename(entry)}>{t("rename")}</MenuItem>
                            <MenuItem disabled={entry.pinned} onSelect={() => onDelete(entry)}>
                              {entry.pinned ? t("pinnedCannotDelete") : t("delete")}
                            </MenuItem>
                          </Menu>
                        </div>
                      ) : (
                        <div className="session-row-shell" onContextMenu={openContextMenu}>
                          <div className="session-row session-row-unavailable" aria-disabled="true" title={providerContext || undefined}>{content}</div>
                          <Menu
                            accessibleLabel={t("manageConversation", { name: managementLabel })}
                            label={<Icon name="more" />}
                          >
                            <MenuItem
                              disabled={entry.sourceState !== "invalid"}
                              onSelect={() => onQuarantine(entry)}
                            >
                              {entry.sourceState === "invalid" ? t("quarantineInvalid") : t("noSafeAction")}
                            </MenuItem>
                          </Menu>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ul>
            </section>
          ))}
        </div>
      )}
      {page.nextCursor !== undefined ? (
        <Button className="load-more" type="button" onClick={onLoadMore} disabled={state === "loading_more"}>
          {state === "loading_more" ? t("loading") : t("loadMore")}
        </Button>
      ) : null}
    </div>
  );
}

function openContextMenu(event: MouseEvent<HTMLDivElement>) {
  event.preventDefault();
  const trigger = event.currentTarget.querySelector<HTMLButtonElement>(".sg-menu .sg-popover-trigger");
  if (trigger?.getAttribute("aria-expanded") !== "true") trigger?.click();
}

type HistoryGroup = {
  readonly id: "today" | "yesterday" | "earlier";
  readonly label: "Today" | "Yesterday" | "Earlier";
  readonly entries: CatalogEntry[];
};

export function groupEntries(entries: readonly CatalogEntry[], referenceTimestamp: number): HistoryGroup[] {
  const reference = new Date(referenceTimestamp > 0 ? referenceTimestamp : Date.now());
  const todayStart = new Date(reference.getFullYear(), reference.getMonth(), reference.getDate()).getTime();
  const yesterdayStart = new Date(reference.getFullYear(), reference.getMonth(), reference.getDate() - 1).getTime();
  const grouped: Record<HistoryGroup["id"], CatalogEntry[]> = { today: [], yesterday: [], earlier: [] };

  for (const entry of entries) {
    if (entry.sourceModifiedAtUnixMs >= todayStart) grouped.today.push(entry);
    else if (entry.sourceModifiedAtUnixMs >= yesterdayStart) grouped.yesterday.push(entry);
    else grouped.earlier.push(entry);
  }

  const labels: Record<HistoryGroup["id"], HistoryGroup["label"]> = {
    today: "Today",
    yesterday: "Yesterday",
    earlier: "Earlier",
  };
  return (["today", "yesterday", "earlier"] as const)
    .filter((id) => grouped[id].length > 0)
    .map((id) => ({ id, label: labels[id], entries: grouped[id] }));
}

function sourceLabel(state: CatalogSourceState, t: ReturnType<typeof useLocale>["t"]): string {
  switch (state) {
    case "ready": return t("ready");
    case "oversized": return t("oversized");
    case "scan_budget_exceeded": return t("scanLimited");
    case "unsupported_legacy": return t("unavailable");
    case "invalid": return t("invalid");
  }
}

function formatTime(value: number, locale?: string): string {
  if (value <= 0) return locale === "zh-CN" ? "刚刚" : "just now";
  return new Intl.DateTimeFormat(locale, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function formatRelativeTime(value: number, referenceTimestamp: number, locale?: string): string {
  if (value <= 0) return locale === "zh-CN" ? "刚刚" : "Just now";
  const reference = referenceTimestamp > 0 ? referenceTimestamp : Date.now();
  const elapsedMinutes = Math.max(0, Math.floor((reference - value) / 60_000));
  if (elapsedMinutes < 1) return locale === "zh-CN" ? "刚刚" : "Just now";
  if (elapsedMinutes < 60) return locale === "zh-CN" ? `${elapsedMinutes} 分钟前` : `${elapsedMinutes}m`;
  const elapsedHours = Math.floor(elapsedMinutes / 60);
  if (elapsedHours < 24) return locale === "zh-CN" ? `${elapsedHours} 小时前` : `${elapsedHours}h`;
  return new Intl.DateTimeFormat(locale, { month: "short", day: "numeric" }).format(new Date(value));
}

function formatActivityCount(entry: CatalogEntry, locale: string): string {
  const messageCount = entry.userMessageCount + entry.assistantMessageCount;
  if (locale === "zh-CN") {
    const tools = entry.toolResultCount === 0 ? "" : ` · ${entry.toolResultCount} 个工具结果`;
    return `${messageCount} 条消息${tools}`;
  }
  const messages = `${messageCount} message${messageCount === 1 ? "" : "s"}`;
  if (entry.toolResultCount === 0) return messages;
  return `${messages} · ${entry.toolResultCount} tool${entry.toolResultCount === 1 ? "" : "s"}`;
}
