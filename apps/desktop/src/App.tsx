import { useCallback, useEffect, useState } from "react";

import { desktopBridge, type DesktopBridge } from "./bridge";
import type { WorkspaceSummary } from "./types";

interface AppProps {
  bridge?: DesktopBridge;
}

type LoadState = "loading" | "ready" | "working" | "error";

export function App({ bridge = desktopBridge }: AppProps) {
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [message, setMessage] = useState("Starting the local desktop bridge…");

  const refresh = useCallback(async () => {
    setLoadState("loading");
    setMessage("Checking local workspace connections…");
    try {
      const bootstrap = await bridge.bootstrap();
      setWorkspaces(bootstrap.workspaces);
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
  }, [bridge]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      void bridge
        .bootstrap()
        .then((bootstrap) => {
          setWorkspaces(bootstrap.workspaces);
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
  }, [bridge]);

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
      setWorkspaces((current) => {
        const others = current.filter(
          (workspace) => workspace.id !== selection.workspace?.id,
        );
        return [...others, selection.workspace as WorkspaceSummary];
      });
      setLoadState("ready");
      setMessage(`${selection.workspace.displayName} is ready.`);
    } catch {
      setLoadState("error");
      setMessage(
        "The workspace could not be opened. Check that it contains sigil.toml.",
      );
    }
  };

  const closeWorkspace = async (workspaceId: string) => {
    setLoadState("working");
    setMessage("Closing the workspace server…");
    try {
      setWorkspaces(await bridge.closeWorkspace(workspaceId));
      setLoadState("ready");
      setMessage("Workspace server closed.");
    } catch {
      setLoadState("error");
      setMessage("The workspace server could not be closed cleanly.");
    }
  };

  return (
    <div className="app-shell">
      <header className="topbar">
        <a className="brand" href="#main" aria-label="Sigil desktop home">
          <span className="brand-mark" aria-hidden="true">
            S
          </span>
          <span>
            <strong>Sigil</strong>
            <small>Desktop preview</small>
          </span>
        </a>
        <span className="security-chip">Local HTTP · private bearer</span>
      </header>

      <main id="main" className="workspace-stage">
        <section className="intro" aria-labelledby="desktop-title">
          <p className="eyebrow">TUI-first · native companion</p>
          <h1 id="desktop-title">Choose where you want to work.</h1>
          <p>
            Each workspace gets its own supervised Sigil server. Credentials,
            processes, and local paths stay in the Rust backend.
          </p>
          <div className="primary-actions">
            <button
              className="primary-button"
              type="button"
              onClick={() => void pickWorkspace()}
              disabled={loadState === "working"}
            >
              Choose workspace
            </button>
            {loadState === "error" ? (
              <button className="quiet-button" type="button" onClick={() => void refresh()}>
                Retry bridge
              </button>
            ) : null}
          </div>
        </section>

        <section className="connection-panel" aria-labelledby="connections-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">Runtime ownership</p>
              <h2 id="connections-title">Workspace connections</h2>
            </div>
            <span className="count-badge">{workspaces.length}</span>
          </div>

          {workspaces.length === 0 ? (
            <div className="empty-state">
              <span className="empty-icon" aria-hidden="true">◇</span>
              <p>No workspace server is running.</p>
              <small>Opening a folder never grants generic filesystem access to the renderer.</small>
            </div>
          ) : (
            <ul className="workspace-list">
              {workspaces.map((workspace) => (
                <li key={workspace.id} className="workspace-row">
                  <span className={`status-dot status-${workspace.state}`} aria-hidden="true" />
                  <span className="workspace-copy">
                    <strong>{workspace.displayName}</strong>
                    <small>
                      {workspace.state} · server {workspace.serverVersion}
                    </small>
                  </span>
                  <button
                    className="quiet-button"
                    type="button"
                    onClick={() => void closeWorkspace(workspace.id)}
                    disabled={loadState === "working"}
                    aria-label={`Close ${workspace.displayName}`}
                  >
                    Close
                  </button>
                </li>
              ))}
            </ul>
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
