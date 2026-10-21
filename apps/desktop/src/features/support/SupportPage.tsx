import { useCallback, useEffect, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type { SessionSummary, SupportDoctorReport, SupportStatus } from "../../types";
import { LoadingState, useNotifications } from "../../ui/feedback";
import { Icon, type IconName } from "../../ui/icons";
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
  const [report, setReport] = useState<SupportDoctorReport>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [saving, setSaving] = useState(false);
  const request = useRef(0);

  const load = useCallback(async () => {
    const current = ++request.current;
    setLoading(true);
    setReport(undefined);
    setError(false);
    try {
      const next = await bridge.supportDoctor(workspaceId);
      if (current === request.current) setReport(next);
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
      detail={t("supportDiagnosticsDetail")}
      navigation={{ label: t("backToSettings"), onBack }}
      aside={<div className="support-header-actions">
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

      {loading ? (
        <div className="application-page-loading">
          <LoadingState label={t("runningDiagnostics")} detail={t("runningDiagnosticsDetail")} />
        </div>
      ) : error ? (
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
          <section className={`support-summary support-${report.summary.overallStatus}`} aria-labelledby="support-summary-title">
            <div className="support-summary-lead">
              <span className="support-summary-icon" aria-hidden="true">
                <Icon name={statusIcon(report.summary.overallStatus)} />
              </span>
              <div>
                <p className="eyebrow">{t("diagnosticStatus")}</p>
                <h2 id="support-summary-title">{statusTitle(report.summary.overallStatus, t)}</h2>
                <p>{t("diagnosticBuild", { version: report.version, target: report.target })}</p>
              </div>
            </div>
            <dl className="support-counts">
              <div><dt>{t("diagnosticOk")}</dt><dd>{report.summary.ok}</dd></div>
              <div><dt>{t("diagnosticWarnings")}</dt><dd>{report.summary.warn}</dd></div>
              <div><dt>{t("diagnosticErrors")}</dt><dd>{report.summary.error}</dd></div>
            </dl>
          </section>

          <section className="support-checks" aria-labelledby="support-checks-title">
            <div className="support-section-heading">
              <div>
                <h2 id="support-checks-title">{t("diagnosticChecks")}</h2>
                <p>{t("diagnosticChecksDetail")}</p>
              </div>
              <span>{new Date(report.generatedAtUnixMs).toLocaleString()}</span>
            </div>
            <SupportChecks checks={report.checks.filter((check) => check.status !== "ok")
              .sort((a, b) => (a.status === "error" ? 0 : 1) - (b.status === "error" ? 0 : 1))} />
            {report.checks.some((check) => check.status === "ok") ? (
              <Collapsible label={t("diagnosticPassedChecks", { count: report.checks.filter((check) => check.status === "ok").length })}>
                <SupportChecks checks={report.checks.filter((check) => check.status === "ok")} />
              </Collapsible>
            ) : null}
          </section>

          <Collapsible className="support-checks" label={t("cutoverAuthority")} summary={t("cutoverBlockerCount", { count: report.cutover.blockers.length })}>
            <div className="support-section-heading">
              <div>
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
          </Collapsible>

          <section className="support-privacy" aria-labelledby="support-privacy-title">
            <div className="support-section-heading">
              <div>
                <h2 id="support-privacy-title">{t("supportPrivacy")}</h2>
                <p>{t("supportPrivacyDetail")}</p>
              </div>
              <Icon name="lock" />
            </div>
            <div className="support-privacy-columns">
              <div><h3>{t("supportIncludes")}</h3><ul>{report.privacy.included.map((item) => <li key={item}>{item}</li>)}</ul></div>
              <div><h3>{t("supportExcludes")}</h3><ul>{report.privacy.excluded.map((item) => <li key={item}>{item}</li>)}</ul></div>
            </div>
            {report.privacy.reviewBeforeSharing ? <p className="support-review-note">{t("reviewSupportBeforeSharing")}</p> : null}
          </section>
        </>
      )}
      <Collapsible key={workspaceId} className="support-recovery" label={t("controlLogRecoveryTitle")}>
        <ControlLogRecoveryCard bridge={bridge} workspaceId={workspaceId} session={session} />
      </Collapsible>
    </ApplicationPage>
  );
}

function statusIcon(status: SupportStatus): IconName {
  return status === "ok" ? "check" : "warning";
}

function statusTitle(status: SupportStatus, t: ReturnType<typeof useLocale>["t"]): string {
  switch (status) {
    case "ok": return t("diagnosticHealthy");
    case "warn": return t("diagnosticNeedsAttention");
    case "error": return t("diagnosticHasErrors");
  }
}

function SupportChecks({ checks }: { readonly checks: SupportDoctorReport["checks"] }) {
  const { t } = useLocale();
  if (checks.length === 0) return null;
  return <ul className="support-check-list">
    {checks.map((check, index) => (
      <li key={`${check.name}-${index}`} className={`support-check support-${check.status}`}>
        <span className="support-check-icon" aria-hidden="true"><Icon name={statusIcon(check.status)} /></span>
        <div>
          <div className="support-check-title"><strong>{check.name}</strong><span>{statusTitle(check.status, t)}</span></div>
          <p>{check.summary}</p>
          {check.remediation === undefined ? null : <small>{check.remediation}</small>}
        </div>
      </li>
    ))}
  </ul>;
}
