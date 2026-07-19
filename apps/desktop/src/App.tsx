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
  const [message, setMessage] = useState("Starting the local desktop bridge…");
  const [historyState, setHistoryState] = useState<HistoryState>("idle");
  const [catalog, setCatalog] = useState<CatalogPage>(EMPTY_CATALOG);
  const [searchDraft, setSearchDraft] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [providerFilter, setProviderFilter] = useState("");
  const [sourceFilter, setSourceFilter] = useState<CatalogSourceState | "all">("all");
  const [pinnedOnly, setPinnedOnly] = useState(false);
  const [selectedSession, setSelectedSession] = useState<SessionSummary>();

  const activeWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === activeWorkspaceId),
    [activeWorkspaceId, workspaces],
  );

  const handleConversationNotice = useCallback((notice: string, error = false) => {
    setLoadState(error ? "error" : "ready");
    setMessage(notice);
  }, []);

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
    setMessage("Checking local workspace connections…");
    try {
      const bootstrap = await bridge.bootstrap();
      applyBootstrap(bootstrap);
      setLoadState("ready");
      setMessage(
        bootstrap.workspaces.length === 0
          ? "Choose a workspace to begin."
          : "Local workspace bridge ready.",
      );
    } catch {
      setLoadState("error");
      setMessage("The local desktop bridge could not be started.");
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
            setLoadState("error");
            setMessage(
              `${unavailable.displayName} stopped unexpectedly. Close it and reopen the workspace.`,
            );
          }
        })
        .catch(() => {
          setLoadState("error");
          setMessage("The local desktop bridge is unavailable.");
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
    setSelectedSession(undefined);
    if (activeWorkspaceId === undefined) {
      setCatalog(EMPTY_CATALOG);
      setHistoryState("idle");
      return;
    }
    void loadHistory(activeWorkspaceId);
  }, [activeWorkspaceId, loadHistory]);

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

  const closeWorkspace = async (workspaceId: string) => {
    setLoadState("working");
    setMessage("Closing the workspace server…");
    try {
      const remaining = await bridge.closeWorkspace(workspaceId);
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
      setMessage("Workspace server closed.");
    } catch {
      setLoadState("error");
      setMessage("The workspace server could not be closed cleanly.");
    }
  };

  const createSession = async () => {
    if (activeWorkspaceId === undefined) return;
    setLoadState("working");
    setMessage("Creating a new conversation…");
    try {
      const session = await bridge.createSession(
        activeWorkspaceId,
        "New conversation",
      );
      setSelectedSession(session);
      setLoadState("ready");
      setMessage("New conversation ready.");
      await loadHistory(activeWorkspaceId);
    } catch {
      setLoadState("error");
      setMessage("The conversation could not be created.");
    }
  };

  const openSession = async (entry: CatalogEntry) => {
    if (activeWorkspaceId === undefined || entry.sessionId === undefined) return;
    setLoadState("working");
    setMessage("Opening conversation history…");
    try {
      const session = await bridge.openSession(activeWorkspaceId, {
        sessionRef: entry.sessionRef,
        sessionId: entry.sessionId,
        label: entry.title,
      });
      setSelectedSession(session);
      setLoadState("ready");
      setMessage("Conversation opened from durable history.");
    } catch {
      setLoadState("error");
      setMessage("The conversation could not be reopened from durable history.");
    }
  };

  return (
    <div className="app-shell">
      <header className="topbar">
        <a className="brand" href="#main" aria-label="Sigil desktop home">
          <span className="brand-mark" aria-hidden="true">S</span>
          <span><strong>Sigil</strong><small>Desktop preview</small></span>
        </a>
        <span className="security-chip">Local HTTP · private bearer</span>
      </header>

      <main id="main" className="desktop-stage">
        <aside className="workspace-sidebar" aria-label="Workspaces">
          <div className="sidebar-heading">
            <div><p className="eyebrow">Local runtime</p><h2>Workspaces</h2></div>
            <button
              className="icon-button"
              type="button"
              onClick={() => void pickWorkspace()}
              disabled={loadState === "working"}
              aria-label="Choose workspace"
            >+</button>
          </div>

          {workspaces.length === 0 ? (
            <div className="sidebar-empty">No server is running.</div>
          ) : (
            <ul className="workspace-nav">
              {workspaces.map((workspace) => (
                <li key={workspace.id}>
                  <button
                    className={`workspace-nav-button ${workspace.id === activeWorkspaceId ? "active" : ""}`}
                    type="button"
                    onClick={() => setActiveWorkspaceId(workspace.id)}
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

          <div className="recent-heading"><span>Recent</span><small>Paths stay native</small></div>
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
        </aside>

        <section className="history-shell" aria-labelledby="history-title">
          {activeWorkspace === undefined ? (
            <div className="welcome-state">
              <p className="eyebrow">TUI-first · native companion</p>
              <h1>Choose where you want to work.</h1>
              <p>Each workspace gets its own supervised Sigil server. Local paths and credentials stay in Rust.</p>
              <button className="primary-button" type="button" onClick={() => void pickWorkspace()}>
                Choose workspace
              </button>
            </div>
          ) : (
            <>
              <div className="history-header">
                <div>
                  <p className="eyebrow">{activeWorkspace.displayName}</p>
                  <h1 id="history-title">Conversation history</h1>
                  <p>Rebuilt from durable session records through the local server.</p>
                </div>
                <button className="primary-button" type="button" onClick={() => void createSession()}>
                  New conversation
                </button>
              </div>

              {selectedSession !== undefined ? (
                <>
                  <div className="selected-session" role="status">
                    <span>Conversation ready</span>
                    <strong>{selectedSession.label ?? selectedSession.id}</strong>
                    <small>{selectedSession.runCount} existing runs</small>
                  </div>
                  <ConversationPanel
                    bridge={bridge}
                    workspaceId={activeWorkspace.id}
                    session={selectedSession}
                    onNotice={handleConversationNotice}
                  />
                </>
              ) : null}

              <form
                className="history-filters"
                onSubmit={(event) => {
                  event.preventDefault();
                  setSearchQuery(searchDraft);
                }}
              >
                <label className="search-field">
                  <span className="sr-only">Search conversation history</span>
                  <input
                    value={searchDraft}
                    onChange={(event) => setSearchDraft(event.target.value)}
                    placeholder="Search titles, providers, and models"
                  />
                </label>
                <input
                  className="provider-field"
                  value={providerFilter}
                  onChange={(event) => setProviderFilter(event.target.value)}
                  placeholder="Provider"
                  aria-label="Filter by provider"
                />
                <select
                  value={sourceFilter}
                  onChange={(event) => setSourceFilter(event.target.value as CatalogSourceState | "all")}
                  aria-label="Filter by source state"
                >
                  <option value="all">All states</option>
                  <option value="ready">Ready</option>
                  <option value="oversized">Oversized</option>
                  <option value="scan_budget_exceeded">Scan limited</option>
                  <option value="unsupported_legacy">Unsupported</option>
                  <option value="invalid">Invalid</option>
                </select>
                <label className="check-filter">
                  <input type="checkbox" checked={pinnedOnly} onChange={(event) => setPinnedOnly(event.target.checked)} />
                  Pinned
                </label>
                <button className="quiet-button" type="submit">Search</button>
              </form>

              <HistoryContent
                state={historyState}
                page={catalog}
                onRetry={() => void loadHistory(activeWorkspace.id)}
                onLoadMore={() => {
                  if (catalog.nextCursor !== undefined) void loadHistory(activeWorkspace.id, catalog.nextCursor);
                }}
                onOpen={(entry) => void openSession(entry)}
              />
            </>
          )}
        </section>
      </main>

      <footer className="statusbar" role="status" aria-live="polite">
        <span className={`status-dot status-${loadState === "error" ? "crashed" : "ready"}`} aria-hidden="true" />
        {message}
      </footer>
    </div>
  );
}

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}
