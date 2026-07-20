import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { desktopBridge, type DesktopBridge } from "./bridge";
import { AppearanceMenu } from "./appearance/AppearanceMenu";
import { ThemeProvider, useAppearance } from "./appearance/ThemeProvider";
import { ConversationPanel } from "./ConversationPanel";
import { type HistoryState } from "./HistoryPanel";
import { SessionRail } from "./features/sessions/SessionRail";
import { WorkspaceSwitcher } from "./features/workspaces/WorkspaceSwitcher";
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
import { useMediaQuery } from "./useMediaQuery";
import { Button, Dialog, Drawer } from "./ui/primitives";
import { Icon } from "./ui/icons";

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
  return (
    <ThemeProvider bridge={bridge}>
      <DesktopApp bridge={bridge} />
    </ThemeProvider>
  );
}

function DesktopApp({ bridge }: { readonly bridge: DesktopBridge }) {
  const appearance = useAppearance();
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
  const [navigationOpen, setNavigationOpen] = useState(false);
  const compactNavigation = useMediaQuery("(max-width: 839px)");
  const navigationTriggerRef = useRef<HTMLButtonElement>(null);
  const workspaceSwitcherRef = useRef<HTMLButtonElement>(null);

  const dismissWorkspaceClose = useCallback(() => {
    setPendingWorkspaceClose((pending) => {
      if (pending !== undefined) setMessage(`${pending.displayName} remains open.`);
      return undefined;
    });
  }, []);

  useEffect(() => {
    if (!compactNavigation) setNavigationOpen(false);
  }, [compactNavigation]);

  const activeWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === activeWorkspaceId),
    [activeWorkspaceId, workspaces],
  );

  const applyBootstrap = useCallback(
    (bootstrap: DesktopBootstrap) => {
      appearance.sync(bootstrap.appearance);
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
    },
    [appearance.sync],
  );

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
      setNavigationOpen(false);
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
      setNavigationOpen(false);
      return;
    }
    setLoadState("working");
    setMessage(`Opening ${recent.displayName}…`);
    try {
      const workspace = await bridge.openRecentWorkspace(recent.id);
      rememberOpenWorkspace(workspace);
      setNavigationOpen(false);
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
      setNavigationOpen(false);
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
      setNavigationOpen(false);
      setSessionActionState("idle");
      setSessionMessage(undefined);
    } catch {
      setSessionActionState("error");
      setSessionMessage("This conversation could not be opened. Refresh the list and try again.");
    }
  };

  const navigationContent = activeWorkspace === undefined ? (
    <div className="session-rail-empty">
      <strong>No workspace is open.</strong>
      <p>Open a project to browse its conversations.</p>
      <Button type="button" variant="primary" onClick={() => void pickWorkspace()}>
        Open workspace
      </Button>
    </div>
  ) : (
    <SessionRail
      historyState={historyState}
      catalog={catalog}
      selectedSessionId={selectedSession?.id}
      sessionMessage={sessionMessage}
      sessionError={sessionActionState === "error"}
      searchDraft={searchDraft}
      providerFilter={providerFilter}
      sourceFilter={sourceFilter}
      pinnedOnly={pinnedOnly}
      onSearchDraftChange={setSearchDraft}
      onSearch={() => setSearchQuery(searchDraft)}
      onProviderFilterChange={setProviderFilter}
      onSourceFilterChange={setSourceFilter}
      onPinnedOnlyChange={setPinnedOnly}
      onClearFilters={() => {
        setProviderFilter("");
        setSourceFilter("all");
        setPinnedOnly(false);
      }}
      onRetry={() => void loadHistory(activeWorkspace.id)}
      onLoadMore={() => {
        if (catalog.nextCursor !== undefined) void loadHistory(activeWorkspace.id, catalog.nextCursor);
      }}
      onOpen={(entry) => void openSession(entry)}
    />
  );

  return (
    <div className="app-shell">
      <header className="topbar">
        <Button
          className="pane-toggle navigation-toggle"
          ref={navigationTriggerRef}
          type="button"
          aria-controls="desktop-navigation"
          aria-expanded={navigationOpen}
          onClick={() => setNavigationOpen((open) => !open)}
        >Browse</Button>
        <a className="brand" href="#main" aria-label="Sigil desktop home">
          <span className="brand-mark" aria-hidden="true">S</span>
          <span><strong>Sigil</strong><small>Coding workspace</small></span>
        </a>
        <WorkspaceSwitcher
          workspaces={workspaces}
          recentWorkspaces={recentWorkspaces}
          activeWorkspaceId={activeWorkspaceId}
          busy={loadState === "working"}
          onSelect={(workspaceId) => {
            setActiveWorkspaceId(workspaceId);
            setNavigationOpen(false);
          }}
          onOpenRecent={(recent) => void openRecentWorkspace(recent)}
          onChoose={() => void pickWorkspace()}
          onClose={(workspaceId) => void closeWorkspace(workspaceId)}
          triggerRef={workspaceSwitcherRef}
        />
        <div className="topbar-actions">
          {activeWorkspace === undefined ? null : (
            <Button aria-label="New conversation" type="button" variant="primary" leadingIcon={<Icon name="add" />} disabled={sessionActionState === "working"} onClick={() => void createSession()}>
              New conversation
            </Button>
          )}
          <AppearanceMenu />
        </div>
      </header>

      <main id="main" className="desktop-stage">
        {compactNavigation ? (
          <Drawer
            id="desktop-navigation"
            open={navigationOpen}
            title="Browse conversations"
            side="start"
            returnFocusRef={navigationTriggerRef}
            onOpenChange={setNavigationOpen}
          >
            {navigationContent}
          </Drawer>
        ) : (
          <aside id="desktop-navigation" className="navigation-pane" aria-label="Conversation navigation">
            {navigationContent}
          </aside>
        )}

        <section className="conversation-stage" aria-label="Conversation workspace">
          {activeWorkspace === undefined ? (
            <div className="welcome-state">
              <p className="eyebrow">Start a task</p>
              <h1>Open a workspace to begin.</h1>
              <p>Choose a project, continue a previous conversation, or start a focused coding task.</p>
              <Button type="button" variant="primary" onClick={() => void pickWorkspace()}>Choose workspace</Button>
            </div>
          ) : selectedSession === undefined ? (
            <div className="conversation-empty">
              <p className="eyebrow">{activeWorkspace.displayName}</p>
              <h1>Select a conversation</h1>
              <p>Continue from the list or start a new coding task in this workspace.</p>
            </div>
          ) : (
            <div className="conversation-surface">
              <ConversationPanel bridge={bridge} workspaceId={activeWorkspace.id} session={selectedSession} />
            </div>
          )}
        </section>
      </main>

      <div className="sr-only" role="status" aria-live="polite">{message}</div>
      {workspaceHealthError !== undefined || loadState !== "ready" ? (
        <footer className="statusbar" role={loadState === "error" ? "alert" : "status"} aria-live="polite">
          <span className={`status-dot status-${workspaceHealthError !== undefined || loadState === "error" ? "crashed" : "ready"}`} aria-hidden="true" />
          {workspaceHealthError ?? message}
        </footer>
      ) : null}

      <Dialog
        open={pendingWorkspaceClose !== undefined}
        title={`Close ${pendingWorkspaceClose?.displayName ?? "workspace"}?`}
        description={pendingWorkspaceClose?.message}
        destructive
        returnFocusRef={workspaceSwitcherRef}
        onOpenChange={(open) => {
          if (!open) dismissWorkspaceClose();
        }}
      >
        {pendingWorkspaceClose === undefined ? null : (
          <>
            <p className="destructive-explanation">
              Closing stops the local runtime and interrupts its active runs. File, shell, and remote side effects that already happened are not undone.
            </p>
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus onClick={dismissWorkspaceClose}>
                Keep running
              </Button>
              <Button type="button" variant="danger" onClick={() => void closeWorkspace(pendingWorkspaceClose.id, true)}>
                Close workspace and interrupt runs
              </Button>
            </div>
          </>
        )}
      </Dialog>
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
