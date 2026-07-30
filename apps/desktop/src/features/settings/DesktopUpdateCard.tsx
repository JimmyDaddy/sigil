import { useEffect, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import type { DesktopUpdatePhase, DesktopUpdateSnapshot } from "../../types";
import { StatusIndicator } from "../../ui/feedback";
import { Icon } from "../../ui/icons";
import { Button } from "../../ui/primitives";
import { useLocale } from "../../i18n";

export function DesktopUpdateCard({ bridge }: { readonly bridge: DesktopBridge }) {
  const { t } = useLocale();
  const [snapshot, setSnapshot] = useState<DesktopUpdateSnapshot>();
  const [loadingFailed, setLoadingFailed] = useState(false);
  const [actionBusy, setActionBusy] = useState(false);

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;
    void bridge.updateState()
      .then((next) => {
        if (active) setSnapshot(next);
      })
      .catch(() => {
        if (active) setLoadingFailed(true);
      });
    void bridge.subscribeUpdate((next) => {
      if (active) {
        setSnapshot(next);
        setLoadingFailed(false);
      }
    }).then((stop) => {
      if (active) unsubscribe = stop;
      else stop();
    }).catch(() => {
      if (active) setLoadingFailed(true);
    });
    return () => {
      active = false;
      unsubscribe?.();
    };
  }, [bridge]);

  const runAction = async (action: () => Promise<DesktopUpdateSnapshot | void>) => {
    if (actionBusy) return;
    setActionBusy(true);
    setLoadingFailed(false);
    try {
      const next = await action();
      if (next !== undefined) setSnapshot(next);
    } catch {
      try {
        setSnapshot(await bridge.updateState());
      } catch {
        setLoadingFailed(true);
      }
    } finally {
      setActionBusy(false);
    }
  };

  const phase = snapshot?.phase;
  const status = updateStatus(phase, t);
  const downloadable = phase === "available";
  const restartable = phase === "ready_to_restart";
  const operationRunning =
    phase === "checking" || phase === "downloading" || phase === "installing";

  return (
    <section className="settings-section settings-update" aria-labelledby="settings-update">
      <div className="settings-section-heading">
        <Icon name="download" />
        <div>
          <h2 id="settings-update">{t("desktopUpdates")}</h2>
          <p>{t("desktopUpdatesDetail")}</p>
        </div>
      </div>
      <div className="desktop-update-control">
        <div className="desktop-update-status">
          <StatusIndicator label={status.label} tone={status.tone} />
          {snapshot === undefined ? null : (
            <span>{t("desktopUpdateCurrentVersion", { version: snapshot.currentVersion })}</span>
          )}
        </div>
        {snapshot?.version === undefined ? null : (
          <div className="desktop-update-release">
            <strong>{t("desktopUpdateAvailableVersion", { version: snapshot.version })}</strong>
            {snapshot.publishedAt === undefined ? null : <small>{snapshot.publishedAt}</small>}
            {snapshot.notes === undefined ? null : <p>{snapshot.notes}</p>}
          </div>
        )}
        {phase !== "downloading" || snapshot === undefined ? null : (
          <div className="desktop-update-progress" aria-live="polite">
            <progress
              aria-label={t("desktopUpdateDownloadProgress")}
              {...(snapshot.totalBytes === undefined
                ? {}
                : {
                  value: snapshot.downloadedBytes,
                  max: snapshot.totalBytes,
                })}
            />
            <small>
              {snapshot.totalBytes === undefined
                ? formatBytes(snapshot.downloadedBytes)
                : `${formatBytes(snapshot.downloadedBytes)} / ${formatBytes(snapshot.totalBytes)}`}
            </small>
          </div>
        )}
        {restartable ? (
          <p className="desktop-update-restart-note">{t("desktopUpdateRestartLater")}</p>
        ) : null}
        {snapshot?.errorCode === undefined && !loadingFailed ? null : (
          <p className="settings-control-unavailable" role="alert">
            {loadingFailed
              ? t("desktopUpdateStateUnavailable")
              : updateError(snapshot?.errorCode, t)}
          </p>
        )}
        <div className="settings-choice-group">
          <Button
            type="button"
            variant={downloadable || restartable ? "primary" : "secondary"}
            leadingIcon={<Icon name={restartable ? "play" : "download"} />}
            busy={actionBusy || operationRunning}
            disabled={snapshot === undefined || phase === "unsupported"}
            onClick={() => {
              if (downloadable) {
                void runAction(() => bridge.downloadAndInstallUpdate());
              } else if (restartable) {
                void runAction(() => bridge.restartAfterUpdate());
              } else {
                void runAction(() => bridge.checkForUpdate());
              }
            }}
          >
            {updateActionLabel(phase, t)}
          </Button>
        </div>
      </div>
    </section>
  );
}

function updateStatus(
  phase: DesktopUpdatePhase | undefined,
  t: ReturnType<typeof useLocale>["t"],
): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  switch (phase) {
    case "up_to_date": return { label: t("desktopUpdateUpToDate"), tone: "success" };
    case "available": return { label: t("desktopUpdateAvailable"), tone: "warning" };
    case "ready_to_restart": return { label: t("desktopUpdateReady"), tone: "success" };
    case "error": return { label: t("desktopUpdateFailed"), tone: "danger" };
    case "unsupported": return { label: t("desktopUpdateUnsupported"), tone: "neutral" };
    case "checking": return { label: t("desktopUpdateChecking"), tone: "neutral" };
    case "downloading": return { label: t("desktopUpdateDownloading"), tone: "neutral" };
    case "installing": return { label: t("desktopUpdateInstalling"), tone: "neutral" };
    case "idle": return { label: t("desktopUpdateBetaChannel"), tone: "neutral" };
    case undefined: return { label: t("loading"), tone: "neutral" };
  }
}

function updateActionLabel(
  phase: DesktopUpdatePhase | undefined,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (phase) {
    case "available": return t("desktopUpdateDownloadInstall");
    case "downloading": return t("desktopUpdateDownloading");
    case "installing": return t("desktopUpdateInstalling");
    case "ready_to_restart": return t("desktopUpdateRestart");
    case "checking": return t("desktopUpdateChecking");
    case "error": return t("retry");
    default: return t("desktopUpdateCheck");
  }
}

function updateError(
  code: string | undefined,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (code) {
    case "update_signature_invalid": return t("desktopUpdateSignatureInvalid");
    case "update_manifest_invalid": return t("desktopUpdateManifestInvalid");
    case "update_install_authorization_failed": return t("desktopUpdateAuthorizationFailed");
    case "update_restart_blocked": return t("desktopUpdateRestartBlocked");
    case "update_restart_busy": return t("desktopUpdateRestartBusy");
    case "update_run_state_unavailable": return t("desktopUpdateRunStateUnavailable");
    case "update_restart_cleanup_failed": return t("desktopUpdateRestartCleanupFailed");
    case "update_too_large": return t("desktopUpdateTooLarge");
    default: return t("desktopUpdateGenericFailure");
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1_024).toFixed(1)} KiB`;
  return `${(bytes / 1_048_576).toFixed(1)} MiB`;
}
