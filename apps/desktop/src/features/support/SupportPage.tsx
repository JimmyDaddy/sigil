import { useCallback, useEffect, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type { SessionSummary, SupportDoctorReport } from "../../types";
import { LoadingState, useNotifications } from "../../ui/feedback";
import { Icon } from "../../ui/icons";
import { Button, Collapsible } from "../../ui/primitives";
import { ApplicationPage } from "../navigation/ApplicationPage";
import { ControlLogRecoveryCard } from "./ControlLogRecoveryCard";
import { rendererRunTimings } from "./runTimings";

const ISSUE_TRACKER_URL = "https://github.com/JimmyDaddy/sigil/issues";

export function SupportPage({
  bridge,
  workspaceId,
  session,
  onBack,
}: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly session?: SessionSummary;
  readonly onBack: () => void;
}) {
  const { t } = useLocale();
  const { notify } = useNotifications();
  const [diagnosis, setDiagnosis] = useState<{ workspaceId: string; report: SupportDoctorReport }>();
  const report = diagnosis?.workspaceId === workspaceId ? diagnosis.report : undefined;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [saving, setSaving] = useState(false);
  const request = useRef(0);

  const load = useCallback(async () => {
    const current = ++request.current;
    setLoading(true);
    setError(false);
    try {
      const next = await bridge.supportDoctor(workspaceId);
      if (current === request.current) setDiagnosis({ workspaceId, report: next });
    } catch {
      if (current === request.current) setError(true);
    } finally {
      if (current === request.current) setLoading(false);
    }
  }, [bridge, workspaceId]);

  useEffect(() => {
    void load();
    return () => { request.current += 1; };
  }, [load]);

  const saveReport = async () => {
    setSaving(true);
    try {
      const result = await bridge.exportSupportBundle(workspaceId, await rendererRunTimings(workspaceId));
      if (!result.cancelled) {
        notify({
          tone: "success",
          title: t("supportReportSaved"),
          message: result.fileName ?? t("supportReportSavedDetail"),
        });
      }
    } catch {
      notify({
        tone: "error",
        title: t("supportReportSaveFailed"),
        message: t("supportReportSaveFailedDetail"),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <ApplicationPage
      className="support-page"
      eyebrow={t("localSupport")}
      title={t("supportDiagnostics")}
      navigation={{ label: t("backToSettings"), onBack }}
      aside={<div className="support-header-actions">
          {!error || report !== undefined ? (
            <Button type="button" variant="quiet" busy={loading} onClick={() => void load()}>
              {loading ? t("runningDiagnostics") : t("recheckDiagnostics")}
            </Button>
          ) : null}
          <Button
            type="button"
            variant="secondary"
            leadingIcon={<Icon name="external" />}
            onClick={() => void bridge.openExternalUrl(ISSUE_TRACKER_URL)}
          >
            {t("openIssueTracker")}
          </Button>
          <Button
            type="button"
            variant="primary"
            leadingIcon={<Icon name="download" />}
            disabled={saving}
            onClick={() => void saveReport()}
          >
            {saving ? t("savingSupportReport") : t("saveSupportReport")}
          </Button>
        </div>}
    >

      {error && report !== undefined ? <p role="alert">{t("diagnosticsRefreshFailed")}</p> : null}
      {loading && report === undefined ? (
        <div className="application-page-loading">
          <LoadingState label={t("runningDiagnostics")} detail={t("runningDiagnosticsDetail")} />
        </div>
      ) : error && report === undefined ? (
        <section className="support-empty" role="alert">
          <Icon name="warning" />
          <div>
            <h2>{t("diagnosticsUnavailable")}</h2>
            <p>{t("diagnosticsUnavailableDetail")}</p>
          </div>
          <Button type="button" onClick={() => void load()}>{t("retry")}</Button>
        </section>
      ) : report === undefined ? null : (
        <>
          <section className="support-summary" aria-labelledby="support-summary-title">
            <h2 id="support-summary-title">{t("diagnosticReportTitle")}</h2>
            <span className="support-report-time">{new Date(report.generatedAtUnixMs).toLocaleString()}</span>
          </section>

          <Collapsible className="support-technical-details" label={t("diagnosticTechnicalDetails")}
            summary={t("diagnosticObservationCount", { count: report.checks.length })}>
            <p className="support-observation-note">{t("diagnosticObservationDetail")}</p>
            <p className="support-observation-note">{t("diagnosticStatus")}: <code>{report.summary.overallStatus}</code></p>
            <dl className="support-counts">
              <div><dt>{t("diagnosticOk")}</dt><dd>{report.summary.ok}</dd></div>
              <div><dt>{t("diagnosticWarnings")}</dt><dd>{report.summary.warn}</dd></div>
              <div><dt>{t("diagnosticErrors")}</dt><dd>{report.summary.error}</dd></div>
            </dl>

          <section className="support-checks" aria-labelledby="support-checks-title">
            <div className="support-section-heading">
              <div>
                <h2 id="support-checks-title">{t("diagnosticChecks")}</h2>
                <p>{t("diagnosticChecksDetail")}</p>
              </div>
            </div>
            <SupportChecks checks={report.checks} />
          </section>

          <section className="support-checks" aria-labelledby="support-authority-title">
            <div className="support-section-heading">
              <div>
                <h2 id="support-authority-title">{t("cutoverAuthority")}</h2>
                <p>{t("diagnosticBuild", { version: report.version, target: report.target })}</p>
                <p>{t("cutoverStatus", { epoch: report.cutover.epoch, authority: report.cutover.authority })}</p>
              </div>
              <span>{t("cutoverBlockerCount", { count: report.cutover.blockers.length })}</span>
            </div>
            {report.cutover.blockers.length === 0 ? (
              <p>{t("cutoverNoBlockers")}</p>
            ) : (
              <ul>
                {report.cutover.blockers.map((blocker, index) => (
                  <li key={`${blocker.code}-${blocker.adapter ?? "none"}-${index}`}>
                    {blocker.code}{blocker.adapter === undefined ? "" : ` · ${blocker.adapter}`}
                  </li>
                ))}
              </ul>
            )}
          </section>
          </Collapsible>

          <section className="support-privacy" aria-labelledby="support-privacy-title">
            <div className="support-section-heading">
              <div>
                <h2 id="support-privacy-title">{t("supportPrivacy")}</h2>
              </div>
              <Icon name="lock" />
            </div>
            {report.privacy.reviewBeforeSharing ? <p className="support-review-note">{t("reviewSupportBeforeSharing")}</p> : null}
            <Collapsible label={t("supportPrivacyScope")}>
            <p>{t("supportPrivacyDetail")}</p>
            <div className="support-privacy-columns">
              <div><h3>{t("supportIncludes")}</h3><ul>{report.privacy.included.map((item) => <li key={item}>{privacyCategory(item, t)}</li>)}</ul></div>
              <div><h3>{t("supportExcludes")}</h3><ul>{report.privacy.excluded.map((item) => <li key={item}>{privacyCategory(item, t)}</li>)}</ul></div>
            </div>
            {[...report.privacy.included, ...report.privacy.excluded].some((item) => privacyCategories.has(item)) ? <div className="support-privacy-identifiers">
              <h3>{t("supportPrivacyIdentifiers")}</h3>
              <p><strong>{t("supportIncludes")}: </strong><code>{report.privacy.included.join(", ")}</code></p>
              <p><strong>{t("supportExcludes")}: </strong><code>{report.privacy.excluded.join(", ")}</code></p>
            </div> : null}
            </Collapsible>
          </section>
        </>
      )}
      <Collapsible key={workspaceId} className="support-recovery" label={t("controlLogRecoveryTitle")}>
        <ControlLogRecoveryCard bridge={bridge} workspaceId={workspaceId} session={session} />
      </Collapsible>
    </ApplicationPage>
  );
}

function SupportChecks({ checks }: { readonly checks: SupportDoctorReport["checks"] }) {
  const { t } = useLocale();
  if (checks.length === 0) return null;
  return <ul className="support-check-list">
    {checks.map((check, index) => (
      <li key={`${check.name}-${index}`} className="support-check">
        <div>
          <div className="support-check-title"><strong>{check.name}</strong><span aria-label={t("diagnosticStatus")}>{check.status}</span></div>
          <p>{check.summary}</p>
          {check.remediation === undefined ? null : <small>{check.remediation}</small>}
        </div>
      </li>
    ))}
  </ul>;
}

const privacyCategories = new Map<string, Parameters<ReturnType<typeof useLocale>["t"]>[0]>([
  ["build_metadata", "supportCategoryBuild"],
  ["os_arch", "supportCategoryPlatform"],
  ["terminal_family", "supportCategoryTerminal"],
  ["doctor_status_and_redacted_checks", "supportCategoryChecks"],
  ["provider_and_model_labels", "supportCategoryModels"],
  ["mcp_aliases", "supportCategoryMcp"],
  ["capability_and_sandbox_status", "supportCategoryCapabilities"],
  ["conversation_content", "supportCategoryConversation"],
  ["tool_input_output", "supportCategoryTools"],
  ["file_content_and_diff", "supportCategoryFiles"],
  ["config_file_content", "supportCategoryConfig"],
  ["credential_and_environment_names_and_values", "supportCategoryCredentials"],
  ["local_paths_and_private_endpoints", "supportCategoryPaths"],
  ["session_log_content", "supportCategorySessionLog"],
]);

function privacyCategory(value: string, t: ReturnType<typeof useLocale>["t"]): string {
  const key = privacyCategories.get(value);
  return key === undefined ? value : t(key);
}
