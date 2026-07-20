import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";

import { desktopBridge, type DesktopBridge } from "./bridge";
import { AppearanceToggle } from "./appearance/AppearanceToggle";
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
import { Button, Dialog, Drawer, IconButton, TextField, Tooltip } from "./ui/primitives";
import { Icon } from "./ui/icons";
import sigilMarkDark from "../../../assets/logo/sigil-mark-dark-mode.svg";
import sigilMarkLight from "../../../assets/logo/sigil-mark.svg";

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
interface PendingSessionRename {
  entry: CatalogEntry;
  displayName: string;
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
const NAVIGATION_WIDTH_STORAGE_KEY = "sigil.desktop.navigation-width.v1";
const DEFAULT_NAVIGATION_WIDTH = 320;
const MIN_NAVIGATION_WIDTH = 280;
const MAX_NAVIGATION_WIDTH = 480;

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
  const [selectedDurableSessionId, setSelectedDurableSessionId] = useState<string>();
  const [sessionActionState, setSessionActionState] = useState<SessionActionState>("idle");
  const [sessionMessage, setSessionMessage] = useState<string>();
  const [pendingWorkspaceClose, setPendingWorkspaceClose] = useState<PendingWorkspaceClose>();
  const [pendingSessionRename, setPendingSessionRename] = useState<PendingSessionRename>();
  const [pendingSessionDelete, setPendingSessionDelete] = useState<CatalogEntry>();
  const [pendingSessionQuarantine, setPendingSessionQuarantine] = useState<CatalogEntry>();
  const [sessionManagementError, setSessionManagementError] = useState<string>();
  const [navigationOpen, setNavigationOpen] = useState(false);
  const [navigationWidth, setNavigationWidth] = useState(readNavigationWidth);
  const compactNavigation = useMediaQuery("(max-width: 899px)");
  const navigationTriggerRef = useRef<HTMLButtonElement>(null);
  const workspaceSwitcherRef = useRef<HTMLButtonElement>(null);
  const sessionRenameInputRef = useRef<HTMLInputElement>(null);

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
      const readyWorkspace = bootstrap.workspaces.find((workspace) => workspace.state === "ready");
      const mostRecentWorkspace = bootstrap.recentWorkspaces[0];
      if (readyWorkspace === undefined && mostRecentWorkspace !== undefined) {
        try {
          const workspace = await bridge.openRecentWorkspace(mostRecentWorkspace.id);
          setWorkspaces([workspace]);
          setRecentWorkspaces((current) => [
            { id: workspace.id, displayName: workspace.displayName, isOpen: true },
            ...current.filter((candidate) => candidate.id !== workspace.id),
          ]);
          setActiveWorkspaceId(workspace.id);
          setLoadState("ready");
          setMessage(`${workspace.displayName} was restored.`);
          return;
        } catch (error) {
          setLoadState("error");
          setMessage(errorMessage(error) ?? "The last workspace could not be restored. Choose it again to continue.");
          return;
        }
      }
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
        if (append && errorCode(error) === "catalog_stale") {
          try {
            const page = await bridge.catalog(workspaceId, catalogRequest());
            setCatalog(page);
            setHistoryState("ready");
            setSessionMessage("Conversation history refreshed because the list changed.");
            return;
          } catch {
            setHistoryState("error");
            return;
          }
        }
        setHistoryState("error");
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
    setSelectedDurableSessionId(undefined);
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
    } catch (error) {
      setLoadState("error");
      setMessage(errorMessage(error) ?? "The recent workspace could not be reopened.");
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
      setSelectedDurableSessionId(undefined);
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
      setSelectedDurableSessionId(entry.sessionId);
      setNavigationOpen(false);
      setSessionActionState("idle");
      setSessionMessage(undefined);
    } catch {
      setSessionActionState("error");
      setSessionMessage("This conversation could not be opened. Refresh the list and try again.");
    }
  };

  const renameSession = async () => {
    if (activeWorkspaceId === undefined || pendingSessionRename === undefined) return;
    const displayName = pendingSessionRename.displayName.trim();
    if (displayName.length === 0 || displayName.length > 160) {
      setSessionManagementError("Enter a conversation name between 1 and 160 characters.");
      return;
    }
    setSessionActionState("working");
    setSessionManagementError(undefined);
    try {
      await bridge.renameSession(activeWorkspaceId, {
        sessionRef: pendingSessionRename.entry.sessionRef,
        sessionId: pendingSessionRename.entry.sessionId ?? "",
        displayName,
      });
      if (selectedDurableSessionId === pendingSessionRename.entry.sessionId) {
        setSelectedSession((current) => current === undefined ? current : { ...current, label: displayName });
      }
      setPendingSessionRename(undefined);
      setSessionActionState("idle");
      setSessionMessage("Conversation renamed.");
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? "The conversation could not be renamed safely.");
    }
  };

  const deleteSession = async () => {
    if (activeWorkspaceId === undefined || pendingSessionDelete?.sessionId === undefined) return;
    setSessionActionState("working");
    setSessionManagementError(undefined);
    try {
      await bridge.deleteSession(activeWorkspaceId, {
        sessionRef: pendingSessionDelete.sessionRef,
        sessionId: pendingSessionDelete.sessionId,
      });
      if (selectedDurableSessionId === pendingSessionDelete.sessionId) {
        setSelectedSession(undefined);
        setSelectedDurableSessionId(undefined);
      }
      setPendingSessionDelete(undefined);
      setSessionActionState("idle");
      setSessionMessage("Conversation deleted.");
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? "The conversation could not be deleted safely.");
    }
  };

  const quarantineSession = async () => {
    if (activeWorkspaceId === undefined || pendingSessionQuarantine === undefined) return;
    setSessionActionState("working");
    setSessionManagementError(undefined);
    try {
      await bridge.quarantineSession(activeWorkspaceId, {
        sessionRef: pendingSessionQuarantine.sessionRef,
        sourceBytes: pendingSessionQuarantine.sourceBytes,
        sourceModifiedAtUnixMs: pendingSessionQuarantine.sourceModifiedAtUnixMs,
      });
      setPendingSessionQuarantine(undefined);
      setSessionActionState("idle");
      setSessionMessage("Invalid conversation source moved to quarantine.");
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? "The invalid source could not be quarantined safely.");
    }
  };

  const navigationContent = activeWorkspace === undefined ? null : (
    <SessionRail
      historyState={historyState}
      catalog={catalog}
      selectedSessionId={selectedDurableSessionId}
      sessionMessage={sessionMessage}
      sessionError={sessionActionState === "error"}
      searchDraft={searchDraft}
      providerFilter={providerFilter}
      sourceFilter={sourceFilter}
      pinnedOnly={pinnedOnly}
      onSearchDraftChange={(value) => {
        setSearchDraft(value);
        if (value.trim() === "" && searchQuery !== "") setSearchQuery("");
      }}
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
      onRename={(entry) => {
        setSessionActionState("idle");
        setSessionManagementError(undefined);
        setPendingSessionRename({ entry, displayName: entry.title ?? "Untitled conversation" });
      }}
      onDelete={(entry) => {
        setSessionActionState("idle");
        setSessionManagementError(undefined);
        setPendingSessionDelete(entry);
      }}
      onQuarantine={(entry) => {
        setSessionActionState("idle");
        setSessionManagementError(undefined);
        setPendingSessionQuarantine(entry);
      }}
    />
  );

  return (
    <div className="app-shell">
      <header className="topbar">
        {activeWorkspace === undefined ? null : (
          <Tooltip label="Browse conversations">
            <IconButton
              className="pane-toggle navigation-toggle"
              ref={navigationTriggerRef}
              type="button"
              icon={<Icon name="menu" />}
              aria-label="Browse conversations"
              aria-controls="desktop-navigation"
              aria-expanded={navigationOpen}
              onClick={() => setNavigationOpen((open) => !open)}
            />
          </Tooltip>
        )}
        <a className="brand" href="#main" aria-label="Sigil desktop home">
          <span className="brand-mark" aria-hidden="true">
            <img className="brand-mark-light" src={sigilMarkLight} alt="" />
            <img className="brand-mark-dark" src={sigilMarkDark} alt="" />
          </span>
          <strong>Sigil</strong>
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
            <Tooltip label="New conversation">
              <IconButton
                className="sg-icon-button-primary"
                aria-label="New conversation"
                type="button"
                icon={<Icon name="add" />}
                disabled={sessionActionState === "working"}
                onClick={() => void createSession()}
              />
            </Tooltip>
          )}
          <AppearanceToggle />
        </div>
      </header>

      <main
        id="main"
        className={`desktop-stage${activeWorkspace === undefined ? " desktop-stage-empty" : ""}`}
        style={{ "--sg-sys-navigation-width": `${navigationWidth}px` } as CSSProperties}
      >
        {activeWorkspace === undefined ? null : compactNavigation ? (
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
            <NavigationResizeHandle width={navigationWidth} onWidthChange={setNavigationWidth} />
          </aside>
        )}

        <section className="conversation-stage" aria-label="Conversation workspace">
          {activeWorkspace === undefined ? (
            <div className="welcome-state">
              <span className="welcome-mark" aria-hidden="true">
                <img className="brand-mark-light" src={sigilMarkLight} alt="" />
                <img className="brand-mark-dark" src={sigilMarkDark} alt="" />
              </span>
              <h1>Open a workspace</h1>
              <p>Choose a project to start or continue a coding conversation.</p>
              <Button type="button" variant="primary" onClick={() => void pickWorkspace()}>Open workspace</Button>
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
        open={pendingSessionRename !== undefined}
        title="Rename conversation"
        description="Use a short name that makes this conversation easy to find."
        initialFocusRef={sessionRenameInputRef}
        onOpenChange={(open) => {
          if (!open && sessionActionState !== "working") {
            setPendingSessionRename(undefined);
            setSessionActionState("idle");
            setSessionManagementError(undefined);
          }
        }}
      >
        {pendingSessionRename === undefined ? null : (
          <form onSubmit={(event) => { event.preventDefault(); void renameSession(); }}>
            <TextField
              ref={sessionRenameInputRef}
              label="Conversation name"
              value={pendingSessionRename.displayName}
              maxLength={160}
              error={sessionManagementError}
              onChange={(event) => setPendingSessionRename((current) => current === undefined ? current : { ...current, displayName: event.target.value })}
            />
            <div className="confirmation-actions">
              <Button type="button" disabled={sessionActionState === "working"} onClick={() => {
                setPendingSessionRename(undefined);
                setSessionActionState("idle");
                setSessionManagementError(undefined);
              }}>Cancel</Button>
              <Button type="submit" variant="primary" busy={sessionActionState === "working"}>Rename</Button>
            </div>
          </form>
        )}
      </Dialog>

      <Dialog
        open={pendingSessionDelete !== undefined}
        title="Delete conversation?"
        description={pendingSessionDelete?.title ?? "Untitled conversation"}
        destructive
        onOpenChange={(open) => {
          if (!open && sessionActionState !== "working") {
            setPendingSessionDelete(undefined);
            setSessionActionState("idle");
            setSessionManagementError(undefined);
          }
        }}
      >
        {pendingSessionDelete === undefined ? null : (
          <>
            <p className="destructive-explanation">
              This permanently removes the local conversation history. File, shell, and remote side effects are not undone.
            </p>
            {sessionManagementError === undefined ? null : <p role="alert">{sessionManagementError}</p>}
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus disabled={sessionActionState === "working"} onClick={() => {
                setPendingSessionDelete(undefined);
                setSessionActionState("idle");
                setSessionManagementError(undefined);
              }}>Keep conversation</Button>
              <Button type="button" variant="danger" busy={sessionActionState === "working"} onClick={() => void deleteSession()}>Delete permanently</Button>
            </div>
          </>
        )}
      </Dialog>

      <Dialog
        open={pendingSessionQuarantine !== undefined}
        title="Quarantine invalid conversation source?"
        description={pendingSessionQuarantine?.sessionRef}
        destructive
        onOpenChange={(open) => {
          if (!open && sessionActionState !== "working") {
            setPendingSessionQuarantine(undefined);
            setSessionActionState("idle");
            setSessionManagementError(undefined);
          }
        }}
      >
        {pendingSessionQuarantine === undefined ? null : (
          <>
            <p className="destructive-explanation">
              Sigil will revalidate this malformed source and move it out of the active conversation list. It is kept in the local quarantine directory for manual recovery.
            </p>
            {sessionManagementError === undefined ? null : <p role="alert">{sessionManagementError}</p>}
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus disabled={sessionActionState === "working"} onClick={() => {
                setPendingSessionQuarantine(undefined);
                setSessionActionState("idle");
                setSessionManagementError(undefined);
              }}>Keep source</Button>
              <Button type="button" variant="danger" busy={sessionActionState === "working"} onClick={() => void quarantineSession()}>Move to quarantine</Button>
            </div>
          </>
        )}
      </Dialog>

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

function NavigationResizeHandle({
  width,
  onWidthChange,
}: {
  readonly width: number;
  readonly onWidthChange: (width: number) => void;
}) {
  const startResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    const handle = event.currentTarget;
    const startX = event.clientX;
    const startWidth = width;
    handle.setPointerCapture(event.pointerId);
    const move = (moveEvent: PointerEvent) => {
      onWidthChange(clampNavigationWidth(startWidth + moveEvent.clientX - startX));
    };
    const finish = (upEvent: PointerEvent) => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
      if (handle.hasPointerCapture(upEvent.pointerId)) handle.releasePointerCapture(upEvent.pointerId);
      persistNavigationWidth(clampNavigationWidth(startWidth + upEvent.clientX - startX));
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
  };

  const setAndPersist = (nextWidth: number) => {
    const bounded = clampNavigationWidth(nextWidth);
    onWidthChange(bounded);
    persistNavigationWidth(bounded);
  };

  return (
    <div
      className="navigation-resize-handle"
      role="separator"
      aria-label="Resize conversation sidebar"
      aria-orientation="vertical"
      aria-valuemin={MIN_NAVIGATION_WIDTH}
      aria-valuemax={MAX_NAVIGATION_WIDTH}
      aria-valuenow={width}
      tabIndex={0}
      onPointerDown={startResize}
      onDoubleClick={() => setAndPersist(DEFAULT_NAVIGATION_WIDTH)}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft") {
          event.preventDefault();
          setAndPersist(width - 16);
        } else if (event.key === "ArrowRight") {
          event.preventDefault();
          setAndPersist(width + 16);
        } else if (event.key === "Home") {
          event.preventDefault();
          setAndPersist(MIN_NAVIGATION_WIDTH);
        } else if (event.key === "End") {
          event.preventDefault();
          setAndPersist(MAX_NAVIGATION_WIDTH);
        }
      }}
    />
  );
}

function readNavigationWidth(): number {
  try {
    const value = Number(window.localStorage.getItem(NAVIGATION_WIDTH_STORAGE_KEY));
    return Number.isFinite(value) && value > 0
      ? clampNavigationWidth(value)
      : DEFAULT_NAVIGATION_WIDTH;
  } catch {
    return DEFAULT_NAVIGATION_WIDTH;
  }
}

function persistNavigationWidth(width: number) {
  try {
    window.localStorage.setItem(NAVIGATION_WIDTH_STORAGE_KEY, String(width));
  } catch {
    // Presentation preferences may be unavailable in hardened webviews.
  }
}

function clampNavigationWidth(width: number): number {
  return Math.min(MAX_NAVIGATION_WIDTH, Math.max(MIN_NAVIGATION_WIDTH, Math.round(width)));
}

function errorCode(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("code" in error)) return undefined;
  return typeof error.code === "string" ? error.code : undefined;
}

function errorMessage(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("message" in error)) return undefined;
  return typeof error.message === "string" ? error.message : undefined;
}
