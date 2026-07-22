import { useCallback, useEffect, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type { SupportDoctorReport, SupportStatus } from "../../types";
import { LoadingState, useNotifications } from "../../ui/feedback";
import { Icon, type IconName } from "../../ui/icons";
import { Button } from "../../ui/primitives";
import { ApplicationPage } from "../navigation/ApplicationPage";

const ISSUE_TRACKER_URL = "https://github.com/JimmyDaddy/sigil/issues";

export function SupportPage({
  bridge,
  workspaceId,
  onBack,
}: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly onBack: () => void;
}) {
  const { t } = useLocale();
  const { notify } = useNotifications();
  const [report, setReport] = useState<SupportDoctorReport>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(false);
    try {
      setReport(await bridge.supportDoctor(workspaceId));
    } catch {
      setReport(undefined);
      setError(true);
    } finally {
      setLoading(false);
    }
  }, [bridge, workspaceId]);

  useEffect(() => { void load(); }, [load]);

  const saveReport = async () => {
    setSaving(true);
    try {
      const result = await bridge.exportSupportBundle(workspaceId);
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
            disabled={saving || report === undefined}
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
            <ul>
              {report.checks.map((check, index) => (
                <li key={`${check.name}-${index}`} className={`support-check support-${check.status}`}>
                  <span className="support-check-icon" aria-hidden="true"><Icon name={statusIcon(check.status)} /></span>
                  <div>
                    <div className="support-check-title"><strong>{check.name}</strong><span>{statusTitle(check.status, t)}</span></div>
                    <p>{check.summary}</p>
                    {check.remediation === undefined ? null : <small>{check.remediation}</small>}
                  </div>
                </li>
              ))}
            </ul>
          </section>

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
