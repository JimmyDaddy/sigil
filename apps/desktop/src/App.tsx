import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";

import { desktopBridge, type DesktopBridge } from "./bridge";
import { ThemeProvider, useAppearance } from "./appearance/ThemeProvider";
import { ConversationPanel } from "./ConversationPanel";
import { LocaleProvider, useLocale } from "./i18n";
import { type HistoryState } from "./HistoryPanel";
import { SessionRail } from "./features/sessions/SessionRail";
import { ConversationLibrary } from "./features/sessions/ConversationLibrary";
import { SettingsPage } from "./features/settings/SettingsPage";
import { SupportPage } from "./features/support/SupportPage";
import { DESKTOP_ROUTE_MAP, useDesktopRouter } from "./features/navigation/useDesktopRouter";
import { WorkspaceSwitcher } from "./features/workspaces/WorkspaceSwitcher";
import { readDefaultModel, readReopenLastWorkspace } from "./preferences";
import type {
  CatalogEntry,
  CatalogPage,
  CatalogRequest,
  CatalogSourceState,
  DesktopBootstrap,
  RecentWorkspaceSummary,
  RunContext,
  SessionSummary,
  WorkspaceSummary,
} from "./types";
import { useMediaQuery } from "./useMediaQuery";
import { Button, Dialog, Drawer, IconButton, TextField, Tooltip } from "./ui/primitives";
import { LoadingState, NotificationProvider, useNotifications } from "./ui/feedback";
import { Icon } from "./ui/icons";
import sigilMarkDark from "../../../assets/logo/sigil-mark-dark-mode.svg";
import sigilMarkLight from "../../../assets/logo/sigil-mark.svg";

interface AppProps {
  bridge?: DesktopBridge;
}

type LoadState = "loading" | "ready" | "working" | "error";
type SessionActionState = "idle" | "working" | "error";
type ConversationNavigationState =
  | { readonly kind: "creating"; readonly targetSessionId?: string }
  | { readonly kind: "opening"; readonly sessionRef: string; readonly title: string; readonly targetSessionId?: string };
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
    <LocaleProvider>
      <ThemeProvider bridge={bridge}>
        <NotificationProvider>
          <DesktopApp bridge={bridge} />
        </NotificationProvider>
      </ThemeProvider>
    </LocaleProvider>
  );
}

function DesktopApp({ bridge }: { readonly bridge: DesktopBridge }) {
  const appearance = useAppearance();
  const { t } = useLocale();
  const { notify } = useNotifications();
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [recentWorkspaces, setRecentWorkspaces] = useState<RecentWorkspaceSummary[]>([]);
  const [activeWorkspaceId, setActiveWorkspaceId] = useState<string>();
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [message, setMessage] = useState(() => t("startingSigil"));
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
  const [workspaceRunContext, setWorkspaceRunContext] = useState<RunContext>();
  const [defaultModel, setDefaultModel] = useState<string>();
  const [sessionActionState, setSessionActionState] = useState<SessionActionState>("idle");
  const [conversationNavigation, setConversationNavigation] = useState<ConversationNavigationState>();
  const [sessionMessage, setSessionMessage] = useState<string>();
  const [pendingWorkspaceClose, setPendingWorkspaceClose] = useState<PendingWorkspaceClose>();
  const [pendingSessionRename, setPendingSessionRename] = useState<PendingSessionRename>();
  const [pendingSessionDelete, setPendingSessionDelete] = useState<CatalogEntry>();
  const [pendingInvalidSourceDelete, setPendingInvalidSourceDelete] = useState<CatalogEntry>();
  const [pendingSessionQuarantine, setPendingSessionQuarantine] = useState<CatalogEntry>();
  const [sessionManagementError, setSessionManagementError] = useState<string>();
  const [navigationOpen, setNavigationOpen] = useState(false);
  const { route: desktopView, navigate, back } = useDesktopRouter();
  const primaryDesktopView = DESKTOP_ROUTE_MAP[desktopView].primary;
  const [navigationWidth, setNavigationWidth] = useState(readNavigationWidth);
  const compactNavigation = useMediaQuery("(max-width: 899px)");
  const navigationTriggerRef = useRef<HTMLButtonElement>(null);
  const sessionSearchRef = useRef<HTMLInputElement>(null);
  const workspaceSwitcherRef = useRef<HTMLButtonElement>(null);
  const sessionRenameInputRef = useRef<HTMLInputElement>(null);

  const dismissWorkspaceClose = useCallback(() => {
    if (pendingWorkspaceClose !== undefined) {
      const retainedMessage = t("workspaceRemainsOpen", { name: pendingWorkspaceClose.displayName });
      setMessage(retainedMessage);
    }
    setPendingWorkspaceClose(undefined);
  }, [pendingWorkspaceClose, t]);

  useEffect(() => {
    if (!compactNavigation) setNavigationOpen(false);
  }, [compactNavigation]);

  const activeWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === activeWorkspaceId),
    [activeWorkspaceId, workspaces],
  );

  useEffect(() => {
    if (activeWorkspace === undefined && DESKTOP_ROUTE_MAP[desktopView].requiresWorkspace) {
      navigate("conversation");
    }
  }, [activeWorkspace, desktopView, navigate]);

  const finishConversationNavigation = useCallback((sessionId: string) => {
    setConversationNavigation((current) =>
      current?.targetSessionId === sessionId ? undefined : current,
    );
  }, []);

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
    setMessage(t("checkingWorkspaces"));
    try {
      const bootstrap = await bridge.bootstrap();
      applyBootstrap(bootstrap);
      const readyWorkspace = bootstrap.workspaces.find((workspace) => workspace.state === "ready");
      const mostRecentWorkspace = bootstrap.recentWorkspaces[0];
      if (
        readyWorkspace === undefined
        && mostRecentWorkspace !== undefined
        && readReopenLastWorkspace()
      ) {
        try {
          const workspace = await bridge.openRecentWorkspace(mostRecentWorkspace.id);
          setWorkspaces([workspace]);
          setRecentWorkspaces((current) => [
            { id: workspace.id, displayName: workspace.displayName, isOpen: true },
            ...current.filter((candidate) => candidate.id !== workspace.id),
          ]);
          setActiveWorkspaceId(workspace.id);
          setLoadState("ready");
          setMessage(t("workspaceRestored", { name: workspace.displayName }));
          return;
        } catch (error) {
          setLoadState("error");
          setMessage(errorMessage(error) ?? t("lastWorkspaceRestoreFailed"));
          return;
        }
      }
      setLoadState("ready");
      setMessage(
        bootstrap.workspaces.length === 0
          ? t("chooseWorkspaceBegin")
          : t("sigilReady"),
      );
    } catch {
      setLoadState("error");
      setMessage(t("sigilStartFailed"));
    }
  }, [applyBootstrap, bridge, t]);

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
              t("workspaceStopped", { name: unavailable.displayName }),
            );
          } else {
            setWorkspaceHealthError(undefined);
          }
        })
        .catch(() => {
          setWorkspaceHealthError(t("workspaceServiceUnavailable"));
        });
    }, 2_000);
    return () => window.clearInterval(timer);
  }, [applyBootstrap, bridge, t]);

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
        return page;
      } catch (error) {
        if (append && errorCode(error) === "catalog_stale") {
          try {
            const page = await bridge.catalog(workspaceId, catalogRequest());
            setCatalog(page);
            setHistoryState("ready");
            setSessionMessage(undefined);
            return page;
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
    setConversationNavigation(undefined);
    setSessionMessage(undefined);
    setWorkspaceRunContext(undefined);
    setDefaultModel(activeWorkspaceId === undefined ? undefined : readDefaultModel(activeWorkspaceId));
  }, [activeWorkspaceId]);

  const captureRunContext = useCallback((context: RunContext) => {
    setWorkspaceRunContext(context);
    setDefaultModel((current) =>
      current === undefined || context.availableModels.includes(current) ? current : undefined,
    );
  }, []);

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
    setMessage(t("waitingWorkspaceSelection"));
    try {
      const selection = await bridge.pickWorkspace();
      if (selection.cancelled || selection.workspace === undefined) {
        setLoadState("ready");
        const cancelledMessage = t("workspaceSelectionCancelled");
        setMessage(cancelledMessage);
        return;
      }
      rememberOpenWorkspace(selection.workspace);
      setNavigationOpen(false);
      setLoadState("ready");
      const readyMessage = t("workspaceReady", { name: selection.workspace.displayName });
      setMessage(readyMessage);
    } catch {
      setLoadState("error");
      setMessage(
        t("workspaceOpenFailed"),
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
    setMessage(t("openingWorkspace", { name: recent.displayName }));
    try {
      const workspace = await bridge.openRecentWorkspace(recent.id);
      rememberOpenWorkspace(workspace);
      setNavigationOpen(false);
      setLoadState("ready");
      const readyMessage = t("workspaceReady", { name: workspace.displayName });
      setMessage(readyMessage);
    } catch (error) {
      setLoadState("error");
      setMessage(errorMessage(error) ?? t("recentWorkspaceOpenFailed"));
    }
  };

  const closeWorkspace = async (workspaceId: string, confirmed = false) => {
    setLoadState("working");
    setMessage(t("closingWorkspace"));
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
      const closedMessage = t("workspaceClosed");
      setMessage(closedMessage);
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
          displayName: workspace?.displayName ?? t("workspace"),
          message: errorMessage(error) ?? t("activeRunOwnsWorkspace"),
        });
        setLoadState("ready");
        setMessage(t("workspaceCloseNeedsConfirmation"));
        return;
      }
      setLoadState("error");
      setMessage(t("workspaceCloseFailed"));
    }
  };

  const createSession = async (modelName?: string): Promise<boolean> => {
    if (activeWorkspaceId === undefined) return false;
    setSessionActionState("working");
    setConversationNavigation({ kind: "creating" });
    setSessionMessage(undefined);
    try {
      const session = await bridge.createSession(
        activeWorkspaceId,
        t("newConversation"),
        modelName ?? defaultModel,
      );
      setConversationNavigation((current) =>
        current?.kind === "creating" ? { ...current, targetSessionId: session.id } : current,
      );
      setSelectedSession(session);
      setSelectedDurableSessionId(session.id);
      setNavigationOpen(false);
      setSessionActionState("idle");
      setSessionMessage(undefined);
      await loadHistory(activeWorkspaceId);
      return true;
    } catch {
      setSessionActionState("error");
      setSessionMessage(t("conversationCreateFailed"));
      setConversationNavigation(undefined);
      return false;
    }
  };

  const openSession = async (entry: CatalogEntry) => {
    if (activeWorkspaceId === undefined || entry.sessionId === undefined) return;
    if (entry.sessionId === selectedDurableSessionId && selectedSession !== undefined) {
      setNavigationOpen(false);
      navigate("conversation");
      return;
    }
    setSessionActionState("working");
    setConversationNavigation({
      kind: "opening",
      sessionRef: entry.sessionRef,
      title: entry.title ?? t("untitledConversation"),
    });
    setSessionMessage(undefined);
    try {
      const session = await bridge.openSession(activeWorkspaceId, {
        sessionRef: entry.sessionRef,
        sessionId: entry.sessionId,
        label: entry.title,
      });
      setConversationNavigation((current) =>
        current?.kind === "opening" && current.sessionRef === entry.sessionRef
          ? { ...current, targetSessionId: session.id }
          : current,
      );
      setSelectedSession(session);
      setSelectedDurableSessionId(entry.sessionId);
      setNavigationOpen(false);
      setSessionActionState("idle");
      setSessionMessage(undefined);
    } catch {
      setSessionActionState("error");
      setSessionMessage(t("conversationOpenFailed"));
      setConversationNavigation(undefined);
    }
  };

  const renameSession = async () => {
    if (activeWorkspaceId === undefined || pendingSessionRename === undefined) return;
    const displayName = pendingSessionRename.displayName.trim();
    if (displayName.length === 0 || displayName.length > 160) {
      setSessionManagementError(t("conversationNameValidation"));
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
      setSessionMessage(undefined);
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? t("conversationRenameFailed"));
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
      setSessionMessage(undefined);
      notify({ message: t("conversationDeleted"), tone: "success" });
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? t("conversationDeleteFailed"));
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
      setSessionMessage(undefined);
      notify({ message: t("invalidSourceQuarantined"), tone: "success" });
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? t("invalidSourceQuarantineFailed"));
    }
  };

  const deleteInvalidSessionSource = async () => {
    if (activeWorkspaceId === undefined || pendingInvalidSourceDelete === undefined) return;
    setSessionActionState("working");
    setSessionManagementError(undefined);
    try {
      await bridge.deleteInvalidSessionSource(activeWorkspaceId, {
        sessionRef: pendingInvalidSourceDelete.sessionRef,
        sourceBytes: pendingInvalidSourceDelete.sourceBytes,
        sourceModifiedAtUnixMs: pendingInvalidSourceDelete.sourceModifiedAtUnixMs,
      });
      setPendingInvalidSourceDelete(undefined);
      setSessionActionState("idle");
      setSessionMessage(undefined);
      notify({ message: t("invalidSourceDeleted"), tone: "success" });
      await loadHistory(activeWorkspaceId);
    } catch (error) {
      setSessionActionState("error");
      setSessionManagementError(errorMessage(error) ?? t("invalidSourceDeleteFailed"));
    }
  };

  const navigationContent = activeWorkspace === undefined ? null : (
    <SessionRail
      historyState={historyState}
      catalog={catalog}
      selectedSessionId={selectedDurableSessionId}
      navigationBusy={conversationNavigation !== undefined}
      openingSessionRef={conversationNavigation?.kind === "opening" ? conversationNavigation.sessionRef : undefined}
      sessionErrorMessage={sessionActionState === "error" ? sessionMessage : undefined}
      searchDraft={searchDraft}
      searchInputRef={sessionSearchRef}
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
        setPendingSessionRename({ entry, displayName: entry.title ?? t("untitledConversation") });
      }}
      onDelete={(entry) => {
        setSessionActionState("idle");
        setSessionManagementError(undefined);
        setPendingSessionDelete(entry);
      }}
      onDeleteInvalidSource={(entry) => {
        setSessionActionState("idle");
        setSessionManagementError(undefined);
        setPendingInvalidSourceDelete(entry);
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
          <Tooltip label={t("browseConversations")}>
            <IconButton
              className="pane-toggle navigation-toggle"
              ref={navigationTriggerRef}
              type="button"
              icon={<Icon name="menu" />}
              aria-label={t("browseConversations")}
              aria-controls="desktop-navigation"
              aria-expanded={navigationOpen}
              onClick={() => setNavigationOpen((open) => !open)}
            />
          </Tooltip>
        )}
        <a
          className="brand"
          href="#main"
          aria-label={t("sigilDesktopHome")}
          aria-current={primaryDesktopView === "conversation" ? "page" : undefined}
          onClick={() => navigate("conversation")}
        >
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
            navigate("conversation");
            setNavigationOpen(false);
          }}
          onOpenRecent={(recent) => void openRecentWorkspace(recent)}
          onChoose={() => void pickWorkspace()}
          onClose={(workspaceId) => void closeWorkspace(workspaceId)}
          triggerRef={workspaceSwitcherRef}
        />
        <div className="topbar-actions">
          {activeWorkspace === undefined ? null : (
            <Tooltip label={t("newConversation")}>
              <IconButton
                className="sg-icon-button-primary"
                aria-label={t("newConversation")}
                type="button"
                icon={<Icon name="add" />}
                disabled={sessionActionState === "working" || conversationNavigation !== undefined}
                onClick={() => { navigate("conversation"); void createSession(); }}
              />
            </Tooltip>
          )}
          {activeWorkspace === undefined ? null : (
            <Tooltip label={t("openConversationLibrary")}>
              <IconButton
                aria-label={t("openConversationLibrary")}
                aria-current={primaryDesktopView === "library" ? "page" : undefined}
                type="button"
                icon={<Icon name="library" />}
                onClick={() => { setNavigationOpen(false); navigate("library"); }}
              />
            </Tooltip>
          )}
          <Tooltip label={t("openSettings")}>
            <IconButton
              aria-label={t("openSettings")}
              aria-current={primaryDesktopView === "settings" ? "page" : undefined}
              type="button"
              icon={<Icon name="settings" />}
              onClick={() => { setNavigationOpen(false); navigate("settings"); }}
            />
          </Tooltip>
        </div>
      </header>

      <main
        id="main"
        className={`desktop-stage${activeWorkspace === undefined || desktopView !== "conversation" ? " desktop-stage-empty" : ""}`}
        style={{ "--sg-sys-navigation-width": `${navigationWidth}px` } as CSSProperties}
      >
        {activeWorkspace === undefined || desktopView !== "conversation" ? null : compactNavigation ? (
          <Drawer
            id="desktop-navigation"
            open={navigationOpen}
            title={t("browseConversations")}
            side="start"
            returnFocusRef={navigationTriggerRef}
            onOpenChange={setNavigationOpen}
          >
            {navigationContent}
          </Drawer>
        ) : (
          <aside id="desktop-navigation" className="navigation-pane" aria-label={t("conversationNavigation")}>
            {navigationContent}
            <NavigationResizeHandle
              width={navigationWidth}
              label={t("resizeConversationSidebar")}
              onWidthChange={setNavigationWidth}
            />
          </aside>
        )}

        <section
          className="conversation-stage"
          aria-label={t("conversationWorkspace")}
          aria-busy={conversationNavigation !== undefined || undefined}
        >
          {desktopView === "settings" ? (
            <SettingsPage
              supportAvailable={activeWorkspace !== undefined}
              workspaceId={activeWorkspace?.id}
              modelContext={workspaceRunContext}
              defaultModel={defaultModel}
              onDefaultModelChange={setDefaultModel}
              onBack={back}
              onOpenSupport={() => {
                if (activeWorkspace !== undefined) navigate("support");
              }}
            />
          ) : desktopView === "support" && activeWorkspace !== undefined ? (
            <SupportPage bridge={bridge} workspaceId={activeWorkspace.id} onBack={back} />
          ) : desktopView === "library" && activeWorkspace !== undefined ? (
            <ConversationLibrary
              bridge={bridge}
              workspaceId={activeWorkspace.id}
              onBack={back}
              onOpen={(entry) => {
                navigate("conversation");
                void openSession(entry);
              }}
            />
          ) : activeWorkspace === undefined && (loadState === "loading" || loadState === "working") ? (
            <LoadingState label={message} />
          ) : activeWorkspace === undefined ? (
            <div className="welcome-state">
              <span className="welcome-mark" aria-hidden="true">
                <img className="brand-mark-light" src={sigilMarkLight} alt="" />
                <img className="brand-mark-dark" src={sigilMarkDark} alt="" />
              </span>
              <h1>{t("openWorkspaceTitle")}</h1>
              <p>{t("openWorkspaceDetail")}</p>
              <Button type="button" variant="primary" onClick={() => void pickWorkspace()}>{t("openWorkspace")}</Button>
            </div>
          ) : selectedSession === undefined ? (
            <div className="conversation-empty">
              <p className="eyebrow">{activeWorkspace.displayName}</p>
              <h1>{t("selectConversation")}</h1>
              <p>{t("selectConversationDetail")}</p>
            </div>
          ) : (
            <div className="conversation-surface" inert={conversationNavigation !== undefined || undefined}>
              <ConversationPanel
                bridge={bridge}
                workspaceId={activeWorkspace.id}
                session={selectedSession}
                onInitialLoadComplete={finishConversationNavigation}
                onRunContextChange={captureRunContext}
                onNewSession={() => createSession()}
                onOpenSessionPicker={(query) => {
                  navigate("conversation");
                  setSearchDraft(query);
                  setSearchQuery(query);
                  if (compactNavigation) setNavigationOpen(true);
                  requestAnimationFrame(() => sessionSearchRef.current?.focus());
                }}
                onOpenSettings={() => navigate("settings")}
                onOpenSupport={() => navigate("support")}
              />
            </div>
          )}
          {desktopView === "conversation" && activeWorkspace !== undefined && conversationNavigation !== undefined ? (
            <div className="conversation-loading-overlay">
              <LoadingState
                label={conversationNavigation.kind === "opening" ? t("openingConversation") : t("creatingConversation")}
                detail={conversationNavigation.kind === "opening"
                  ? t("openingConversationDetail", { name: conversationNavigation.title })
                  : t("creatingConversationDetail")}
              />
            </div>
          ) : null}
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
        title={t("renameConversation")}
        description={t("renameConversationDetail")}
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
              label={t("conversationName")}
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
              }}>{t("cancel")}</Button>
              <Button type="submit" variant="primary" busy={sessionActionState === "working"}>{t("rename")}</Button>
            </div>
          </form>
        )}
      </Dialog>

      <Dialog
        open={pendingSessionDelete !== undefined}
        title={t("deleteConversation")}
        description={pendingSessionDelete?.title ?? t("untitledConversation")}
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
              {t("deleteConversationDetail")}
            </p>
            {sessionManagementError === undefined ? null : <p role="alert">{sessionManagementError}</p>}
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus disabled={sessionActionState === "working"} onClick={() => {
                setPendingSessionDelete(undefined);
                setSessionActionState("idle");
                setSessionManagementError(undefined);
              }}>{t("keepConversation")}</Button>
              <Button type="button" variant="danger" busy={sessionActionState === "working"} onClick={() => void deleteSession()}>{t("deletePermanently")}</Button>
            </div>
          </>
        )}
      </Dialog>

      <Dialog
        open={pendingSessionQuarantine !== undefined}
        title={t("quarantineConversation")}
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
              {t("quarantineConversationDetail")}
            </p>
            {sessionManagementError === undefined ? null : <p role="alert">{sessionManagementError}</p>}
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus disabled={sessionActionState === "working"} onClick={() => {
                setPendingSessionQuarantine(undefined);
                setSessionActionState("idle");
                setSessionManagementError(undefined);
              }}>{t("keepSource")}</Button>
              <Button type="button" variant="danger" busy={sessionActionState === "working"} onClick={() => void quarantineSession()}>{t("moveToQuarantine")}</Button>
            </div>
          </>
        )}
      </Dialog>

      <Dialog
        open={pendingInvalidSourceDelete !== undefined}
        title={t("deleteInvalidSourceQuestion")}
        description={pendingInvalidSourceDelete?.sessionRef}
        destructive
        onOpenChange={(open) => {
          if (!open && sessionActionState !== "working") {
            setPendingInvalidSourceDelete(undefined);
            setSessionActionState("idle");
            setSessionManagementError(undefined);
          }
        }}
      >
        {pendingInvalidSourceDelete === undefined ? null : (
          <>
            <p className="destructive-explanation">{t("deleteInvalidSourceDetail")}</p>
            {sessionManagementError === undefined ? null : <p role="alert">{sessionManagementError}</p>}
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus disabled={sessionActionState === "working"} onClick={() => {
                setPendingInvalidSourceDelete(undefined);
                setSessionActionState("idle");
                setSessionManagementError(undefined);
              }}>{t("keepSource")}</Button>
              <Button type="button" variant="danger" busy={sessionActionState === "working"} onClick={() => void deleteInvalidSessionSource()}>{t("deletePermanently")}</Button>
            </div>
          </>
        )}
      </Dialog>

      <Dialog
        open={pendingWorkspaceClose !== undefined}
        title={t("closeWorkspaceQuestion", { name: pendingWorkspaceClose?.displayName ?? t("workspace") })}
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
              {t("closeWorkspaceDetail")}
            </p>
            <div className="confirmation-actions">
              <Button type="button" data-initial-focus onClick={dismissWorkspaceClose}>
                {t("keepRunning")}
              </Button>
              <Button type="button" variant="danger" onClick={() => void closeWorkspace(pendingWorkspaceClose.id, true)}>
                {t("closeAndInterrupt")}
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
  label,
  onWidthChange,
}: {
  readonly width: number;
  readonly label: string;
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
      aria-label={label}
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
