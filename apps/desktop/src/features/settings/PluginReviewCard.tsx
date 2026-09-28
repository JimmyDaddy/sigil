import { useCallback, useEffect, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type { PluginCatalog, PluginReview } from "../../types";
import { Button } from "../../ui/primitives";

export function PluginReviewCard({ bridge, workspaceId, sessionId }: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly sessionId: string;
}) {
  const { t } = useLocale();
  const [catalog, setCatalog] = useState<PluginCatalog>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState(false);
  const [saved, setSaved] = useState(false);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    const request = ++generation.current;
    setPending(true);
    setError(false);
    try {
      const current = await bridge.pluginCatalog(workspaceId, sessionId);
      if (request === generation.current) {
        setCatalog((previous) => ({
          ...current,
          plugins: current.plugins.map((plugin) => {
            const priorCleanup = previous?.plugins.find((prior) => prior.pluginId === plugin.pluginId)?.processCleanup;
            // Absence is not evidence that a previously unconfirmed generation has stopped.
            return plugin.processCleanup == null && (priorCleanup === "unknown" || priorCleanup === "unconfirmed")
              ? { ...plugin, processCleanup: priorCleanup }
              : plugin;
          }),
        }));
      }
    } catch {
      if (request === generation.current) setError(true);
    } finally {
      if (request === generation.current) setPending(false);
    }
  }, [bridge, workspaceId, sessionId]);
  useEffect(() => {
    setCatalog(undefined);
    setSaved(false);
    void refresh();
    return () => { generation.current += 1; };
  }, [refresh]);

  const review = async (plugin: PluginReview, enabled: boolean) => {
    const request = ++generation.current;
    setPending(true);
    setError(false);
    setSaved(false);
    try {
      const receipt = await bridge.commandConversationRecovery(workspaceId, {
        sessionId,
        action: { kind: "review_plugin", pluginId: plugin.pluginId, manifestHash: plugin.manifestHash,
          capabilityDigest: plugin.capabilityDigest, enabled },
      });
      if (request !== generation.current) return;
      if (receipt.pluginReview?.pluginId !== plugin.pluginId || receipt.pluginReview.enabled !== enabled) {
        setError(true);
        return;
      }
      const processCleanup = receipt.pluginReview.processCleanup;
      // A receipt describes this operation; only the canonical catalog can clear older warnings.
      if (processCleanup === "unknown" || processCleanup === "unconfirmed") {
        setCatalog((current) => current && ({
          ...current,
          plugins: current.plugins.map((entry) => entry.pluginId === plugin.pluginId
            ? { ...entry, processCleanup }
            : entry),
        }));
      }
      setSaved(true);
      await refresh();
    } catch {
      if (request === generation.current) setError(true);
    } finally {
      if (request === generation.current) setPending(false);
    }
  };

  return <section className="settings-section" aria-label={t("workspacePlugins")}>
    <header><h2>{t("workspacePlugins")}</h2><p>{t("workspacePluginsDetail")}</p></header>
    <Button type="button" busy={pending} onClick={() => void refresh()}>{t("refreshReview")}</Button>
    {error && <p role="alert">{t("pluginReviewFailed")}</p>}
    {saved && <p role="status">{t("pluginReviewSaved")}</p>}
    {catalog?.warningCount ? <p>{t("pluginDiscoveryWarnings")} ({catalog.warningCount})</p> : null}
    {catalog?.plugins.length === 0 && <p>{t("noWorkspacePlugins")}</p>}
    {catalog?.plugins.map((plugin) => {
      const cleanup = plugin.processCleanup ?? (plugin.trust === "disabled" ? "unknown" : undefined);
      const retryCleanup = plugin.trust === "disabled" && cleanup !== "confirmed";
      return <article key={plugin.pluginId} className="settings-section">
      <h3>{plugin.name || plugin.pluginId} · {plugin.version}</h3>
      <p>{t(plugin.trust === "trusted" ? "pluginTrusted" : plugin.trust === "disabled" ? "pluginDisabled" : "pluginNeedsReview")}</p>
      {cleanup && <p role={cleanup === "confirmed" ? "status" : "alert"}>
        {t(cleanup === "confirmed" ? "pluginCleanupConfirmed" : cleanup === "unconfirmed" ? "pluginCleanupUnconfirmed" : "pluginCleanupUnknown")}
      </p>}
      <ul>{plugin.capabilities.map((capability, index) => <li key={`${capability.kind}:${index}`}>
        {capability.kind}: {capability.label}
        {capability.approval ? ` · ${t("pluginApproval")}: ${capability.approval}` : ""}
        {capability.allowSecrets ? ` · ${t("pluginSecretAccess")}` : ""}
        {capability.egressLogging ? ` · ${t("pluginEgressLogged")}` : ""}
      </li>)}</ul>
      <Button type="button" disabled={pending || plugin.trust === "trusted"} onClick={() => void review(plugin, true)}>{t("trustPlugin")}</Button>
      <Button type="button" disabled={pending || (plugin.trust === "disabled" && cleanup === "confirmed")} onClick={() => void review(plugin, false)}>{t(retryCleanup ? "retryPluginCleanup" : "disablePlugin")}</Button>
    </article>;
    })}
  </section>;
}
