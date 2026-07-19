import { useCallback, useEffect, useMemo, useState } from "react";

import { desktopBridge, type DesktopBridge } from "./bridge";
import { ConversationPanel } from "./ConversationPanel";
import { HistoryContent, type HistoryState } from "./HistoryPanel";
import type {
  CatalogEntry,
  CatalogPage,
  CatalogRequest,
  CatalogSourceState,
  DesktopBootstrap,
  RecentWorkspaceSummary,
  SessionSummary,
  WorkspaceSummary,
} from "./types";

interface AppProps {
  bridge?: DesktopBridge;
}

type LoadState = "loading" | "ready" | "working" | "error";
type SessionActionState = "idle" | "working" | "error";
interface PendingWorkspaceClose {
  id: string;
  displayName: string;
  message: string;
}
const EMPTY_CATALOG: CatalogPage = {
  workspaceId: "",
  generation: 0,
  reconciledAtUnixMs: 0,
  degradedSourceCount: 0,
  identityConflictCount: 0,
  truncatedSourceCount: 0,
  entries: [],
};

export function App({ bridge = desktopBridge }: AppProps) {
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [recentWorkspaces, setRecentWorkspaces] = useState<RecentWorkspaceSummary[]>([]);
  const [activeWorkspaceId, setActiveWorkspaceId] = useState<string>();
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [message, setMessage] = useState("Starting Sigil…");
  const [workspaceHealthError, setWorkspaceHealthError] = useState<string>();
  const [historyState, setHistoryState] = useState<HistoryState>("idle");
  const [catalog, setCatalog] = useState<CatalogPage>(EMPTY_CATALOG);
  const [searchDraft, setSearchDraft] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [providerFilter, setProviderFilter] = useState("");
  const [sourceFilter, setSourceFilter] = useState<CatalogSourceState | "all">("all");
  const [pinnedOnly, setPinnedOnly] = useState(false);
  const [selectedSession, setSelectedSession] = useState<SessionSummary>();
  const [sessionActionState, setSessionActionState] = useState<SessionActionState>("idle");
  const [sessionMessage, setSessionMessage] = useState<string>();
  const [pendingWorkspaceClose, setPendingWorkspaceClose] = useState<PendingWorkspaceClose>();

  const activeWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === activeWorkspaceId),
    [activeWorkspaceId, workspaces],
  );

  const applyBootstrap = useCallback((bootstrap: DesktopBootstrap) => {
    setWorkspaces(bootstrap.workspaces);
    setRecentWorkspaces(bootstrap.recentWorkspaces);
    setActiveWorkspaceId((current) => {
      if (
        current !== undefined &&
        bootstrap.workspaces.some(
          (workspace) => workspace.id === current && workspace.state === "ready",
        )
      ) {
        return current;
      }
      return bootstrap.workspaces.find((workspace) => workspace.state === "ready")?.id;
    });
  }, []);

  const refresh = useCallback(async () => {
    setLoadState("loading");
    setMessage("Checking open workspaces…");
    try {
      const bootstrap = await bridge.bootstrap();
      applyBootstrap(bootstrap);
      setLoadState("ready");
      setMessage(
        bootstrap.workspaces.length === 0
          ? "Choose a workspace to begin."
          : "Sigil is ready.",
      );
    } catch {
      setLoadState("error");
      setMessage("Sigil could not be started.");
    }
  }, [applyBootstrap, bridge]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      void bridge
        .bootstrap()
        .then((bootstrap) => {
          applyBootstrap(bootstrap);
          const unavailable = bootstrap.workspaces.find(
            (workspace) => workspace.state !== "ready",
          );
          if (unavailable !== undefined) {
            setWorkspaceHealthError(
              `${unavailable.displayName} stopped unexpectedly. Close it and reopen the workspace.`,
            );
          } else {
            setWorkspaceHealthError(undefined);
          }
        })
        .catch(() => {
          setWorkspaceHealthError("Sigil cannot reach the workspace service.");
        });
    }, 2_000);
    return () => window.clearInterval(timer);
  }, [applyBootstrap, bridge]);

  const catalogRequest = useCallback(
    (cursor?: string): CatalogRequest => ({
      limit: 30,
      cursor,
      query: searchQuery.trim() || undefined,
      provider: providerFilter.trim() || undefined,
      pinned: pinnedOnly || undefined,
      state: sourceFilter === "all" ? undefined : sourceFilter,
    }),
    [pinnedOnly, providerFilter, searchQuery, sourceFilter],
  );

  const loadHistory = useCallback(
    async (workspaceId: string, cursor?: string) => {
      const append = cursor !== undefined;
      setHistoryState(append ? "loading_more" : "loading");
      try {
        const page = await bridge.catalog(workspaceId, catalogRequest(cursor));
        setCatalog((current) =>
          append
            ? { ...page, entries: [...current.entries, ...page.entries] }
            : page,
        );
        setHistoryState("ready");
      } catch (error) {
        setHistoryState(errorCode(error) === "catalog_stale" ? "stale" : "error");
      }
    },
    [bridge, catalogRequest],
  );

  useEffect(() => {
    if (activeWorkspaceId === undefined) {
      setCatalog(EMPTY_CATALOG);
      setHistoryState("idle");
      return;
    }
    void loadHistory(activeWorkspaceId);
  }, [activeWorkspaceId, loadHistory]);

  useEffect(() => {
    setSelectedSession(undefined);
    setSessionActionState("idle");
    setSessionMessage(undefined);
  }, [activeWorkspaceId]);

  const rememberOpenWorkspace = (workspace: WorkspaceSummary) => {
    setWorkspaces((current) => [
      ...current.filter((candidate) => candidate.id !== workspace.id),
      workspace,
    ]);
    setRecentWorkspaces((current) => [
      { id: workspace.id, displayName: workspace.displayName, isOpen: true },
      ...current.filter((candidate) => candidate.id !== workspace.id),
    ]);
    setActiveWorkspaceId(workspace.id);
  };

  const pickWorkspace = async () => {
    setLoadState("working");
    setMessage("Waiting for a workspace selection…");
    try {
      const selection = await bridge.pickWorkspace();
      if (selection.cancelled || selection.workspace === undefined) {
        setLoadState("ready");
        setMessage("Workspace selection cancelled.");
        return;
      }
      rememberOpenWorkspace(selection.workspace);
      setLoadState("ready");
      setMessage(`${selection.workspace.displayName} is ready.`);
    } catch {
      setLoadState("error");
      setMessage(
        "The workspace could not be opened. Check that it contains sigil.toml.",
      );
    }
  };

  const openRecentWorkspace = async (recent: RecentWorkspaceSummary) => {
    if (recent.isOpen) {
      setActiveWorkspaceId(recent.id);
      return;
    }
    setLoadState("working");
    setMessage(`Opening ${recent.displayName}…`);
    try {
      const workspace = await bridge.openRecentWorkspace(recent.id);
      rememberOpenWorkspace(workspace);
      setLoadState("ready");
      setMessage(`${workspace.displayName} is ready.`);
    } catch {
      setLoadState("error");
      setMessage("The recent workspace could not be reopened.");
    }
  };

  const closeWorkspace = async (workspaceId: string, confirmed = false) => {
    setLoadState("working");
    setMessage("Closing the workspace…");
    try {
      const remaining = await bridge.closeWorkspace(workspaceId, confirmed);
      setWorkspaces(remaining);
      setRecentWorkspaces((current) =>
        current.map((recent) =>
          recent.id === workspaceId ? { ...recent, isOpen: false } : recent,
        ),
      );
      setActiveWorkspaceId((current) =>
        current === workspaceId
          ? remaining.find((workspace) => workspace.state === "ready")?.id
          : current,
      );
      setLoadState("ready");
      setMessage("Workspace closed.");
      setPendingWorkspaceClose(undefined);
    } catch (error) {
      if (
        ["workspace_active_runs", "workspace_run_state_unavailable"].includes(
          errorCode(error) ?? "",
        ) && !confirmed
      ) {
        const workspace = workspaces.find((candidate) => candidate.id === workspaceId);
        setPendingWorkspaceClose({
          id: workspaceId,
          displayName: workspace?.displayName ?? "workspace",
          message: errorMessage(error) ?? "An active run still belongs to this workspace.",
        });
        setLoadState("ready");
        setMessage("Workspace close needs confirmation because a run is active.");
        return;
      }
      setLoadState("error");
      setMessage("The workspace could not be closed cleanly.");
    }
  };

  const createSession = async () => {
    if (activeWorkspaceId === undefined) return;
    setSessionActionState("working");
    setSessionMessage("Creating a new conversation…");
    try {
      const session = await bridge.createSession(
        activeWorkspaceId,
        "New conversation",
      );
      setSelectedSession(session);
      setSessionActionState("idle");
      setSessionMessage("New conversation ready.");
      await loadHistory(activeWorkspaceId);
    } catch {
      setSessionActionState("error");
      setSessionMessage("The conversation could not be created. Try again.");
    }
  };

  const openSession = async (entry: CatalogEntry) => {
    if (activeWorkspaceId === undefined || entry.sessionId === undefined) return;
    setSessionActionState("working");
    setSessionMessage("Opening conversation…");
    try {
      const session = await bridge.openSession(activeWorkspaceId, {
        sessionRef: entry.sessionRef,
        sessionId: entry.sessionId,
        label: entry.title,
      });
      setSelectedSession(session);
      setSessionActionState("idle");
      setSessionMessage(undefined);
    } catch {
      setSessionActionState("error");
      setSessionMessage("This conversation could not be opened. Refresh the list and try again.");
    }
  };

  return (
    <div className="app-shell">
      <header className="topbar">
        <a className="brand" href="#main" aria-label="Sigil desktop home">
          <span className="brand-mark" aria-hidden="true">S</span>
          <span><strong>Sigil</strong><small>Coding workspace</small></span>
        </a>
        <span className="workspace-chip">{activeWorkspace?.displayName ?? "No workspace open"}</span>
      </header>

      <main id="main" className="desktop-stage">
        <aside className="navigation-pane" aria-label="Workspace and conversations">
          <section className="workspace-navigation" aria-labelledby="workspace-title">
            <div className="sidebar-heading">
              <div><p className="eyebrow">Projects</p><h2 id="workspace-title">Workspaces</h2></div>
              <button
                className="icon-button"
                type="button"
                onClick={() => void pickWorkspace()}
                disabled={loadState === "working"}
                aria-label="Choose workspace"
              >+</button>
            </div>
            {workspaces.length === 0 ? (
              <div className="sidebar-empty">No workspace is open.</div>
            ) : (
              <ul className="workspace-nav">
                {workspaces.map((workspace) => (
                  <li key={workspace.id}>
                    <button
                      className={`workspace-nav-button ${workspace.id === activeWorkspaceId ? "active" : ""}`}
                      type="button"
                      onClick={() => setActiveWorkspaceId(workspace.id)}
                      aria-current={workspace.id === activeWorkspaceId ? "page" : undefined}
                    >
                      <span className={`status-dot status-${workspace.state}`} aria-hidden="true" />
                      <span><strong>{workspace.displayName}</strong><small>{workspace.state}</small></span>
                    </button>
                    <button
                      className="row-close"
                      type="button"
                      onClick={() => void closeWorkspace(workspace.id)}
                      aria-label={`Close ${workspace.displayName}`}
                    >×</button>
                  </li>
                ))}
              </ul>
            )}
            <details className="recent-workspaces">
              <summary>Recent workspaces</summary>
              {recentWorkspaces.length === 0 ? (
                <div className="sidebar-empty">No recent workspace yet.</div>
              ) : (
                <ul className="recent-list">
                  {recentWorkspaces.map((recent) => (
                    <li key={recent.id}>
                      <button type="button" onClick={() => void openRecentWorkspace(recent)}>
                        <span>{recent.displayName}</span>
                        <small>{recent.isOpen ? "Open" : "Reopen"}</small>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </details>
          </section>

          {activeWorkspace !== undefined ? (
            <section className="session-navigation" aria-labelledby="history-title">
              <div className="session-navigation-header">
                <div><p className="eyebrow">{activeWorkspace.displayName}</p><h2 id="history-title">Conversations</h2></div>
                <button
                  className="icon-button"
                  type="button"
                  onClick={() => void createSession()}
                  disabled={sessionActionState === "working"}
                  aria-label="Create conversation"
                >+</button>
              </div>
              <form
                className="history-filters"
                onSubmit={(event) => {
                  event.preventDefault();
                  setSearchQuery(searchDraft);
                }}
              >
                <label className="search-field">
                  <span className="sr-only">Search conversation history</span>
                  <input value={searchDraft} onChange={(event) => setSearchDraft(event.target.value)} placeholder="Search conversations" />
                </label>
                <input className="provider-field" value={providerFilter} onChange={(event) => setProviderFilter(event.target.value)} placeholder="Provider" aria-label="Filter by provider" />
                <select value={sourceFilter} onChange={(event) => setSourceFilter(event.target.value as CatalogSourceState | "all")} aria-label="Filter by source state">
                  <option value="all">All states</option>
                  <option value="ready">Ready</option>
                  <option value="oversized">Oversized</option>
                  <option value="scan_budget_exceeded">Scan limited</option>
                  <option value="unsupported_legacy">Unsupported</option>
                  <option value="invalid">Invalid</option>
                </select>
                <label className="check-filter">
                  <input type="checkbox" checked={pinnedOnly} onChange={(event) => setPinnedOnly(event.target.checked)} /> Pinned
                </label>
                <button className="quiet-button" type="submit">Search</button>
              </form>
              {sessionMessage !== undefined ? (
                <div className={`session-notice ${sessionActionState === "error" ? "error" : ""}`} role={sessionActionState === "error" ? "alert" : "status"}>
                  {sessionMessage}
                </div>
              ) : null}
              <div className="session-list-scroll">
                <HistoryContent
                  state={historyState}
                  page={catalog}
                  onRetry={() => void loadHistory(activeWorkspace.id)}
                  onLoadMore={() => {
                    if (catalog.nextCursor !== undefined) void loadHistory(activeWorkspace.id, catalog.nextCursor);
                  }}
                  onOpen={(entry) => void openSession(entry)}
                />
              </div>
            </section>
          ) : null}
        </aside>

        <section className="conversation-stage" aria-label="Conversation workspace">
          {activeWorkspace === undefined ? (
            <div className="welcome-state">
              <p className="eyebrow">Start a task</p>
              <h1>Open a workspace to begin.</h1>
              <p>Choose a project, continue a previous conversation, or start a focused coding task.</p>
              <button className="primary-button" type="button" onClick={() => void pickWorkspace()}>Choose workspace</button>
            </div>
          ) : selectedSession === undefined ? (
            <div className="conversation-empty">
              <p className="eyebrow">{activeWorkspace.displayName}</p>
              <h1>Select a conversation</h1>
              <p>Continue from the list or start a new coding task in this workspace.</p>
              <button className="primary-button" type="button" disabled={sessionActionState === "working"} onClick={() => void createSession()}>
                New conversation
              </button>
            </div>
          ) : (
            <div className="conversation-surface">
              <div className="selected-session">
                <div><p className="eyebrow">{activeWorkspace.displayName}</p><strong>{selectedSession.label ?? "Untitled conversation"}</strong></div>
                <small>{selectedSession.runCount} recorded run{selectedSession.runCount === 1 ? "" : "s"}</small>
              </div>
              <ConversationPanel bridge={bridge} workspaceId={activeWorkspace.id} session={selectedSession} />
            </div>
          )}
        </section>
      </main>

      <footer className="statusbar" role="status" aria-live="polite">
        <span className={`status-dot status-${workspaceHealthError !== undefined || loadState === "error" ? "crashed" : "ready"}`} aria-hidden="true" />
        {workspaceHealthError ?? message}
      </footer>

      {pendingWorkspaceClose !== undefined ? (
        <div className="modal-backdrop">
          <section
            className="confirmation-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="close-workspace-title"
            aria-describedby="close-workspace-description"
          >
            <p className="eyebrow">Active work</p>
            <h2 id="close-workspace-title">Close {pendingWorkspaceClose.displayName}?</h2>
            <p id="close-workspace-description">{pendingWorkspaceClose.message}</p>
            <p>
              Closing stops the local runtime and interrupts its active runs. File, shell, and remote side effects that already happened are not undone.
            </p>
            <div className="confirmation-actions">
              <button
                className="quiet-button"
                type="button"
                autoFocus
                onClick={() => {
                  setPendingWorkspaceClose(undefined);
                  setMessage(`${pendingWorkspaceClose.displayName} remains open.`);
                }}
              >
                Keep running
              </button>
              <button
                className="primary-button danger-button"
                type="button"
                onClick={() => void closeWorkspace(pendingWorkspaceClose.id, true)}
              >
                Close workspace and interrupt runs
              </button>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

function errorMessage(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("message" in error)) return undefined;
  return typeof error.message === "string" ? error.message : undefined;
}
