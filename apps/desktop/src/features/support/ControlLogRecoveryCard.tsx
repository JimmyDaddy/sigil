import { useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type { ControlLogRecoveryAction, ControlLogRecoveryPreview, SessionSummary } from "../../types";
import { Button } from "../../ui/primitives";

export function ControlLogRecoveryCard({ bridge, workspaceId, session }: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly session?: SessionSummary;
}) {
  const { t } = useLocale();
  const [preview, setPreview] = useState<ControlLogRecoveryPreview>();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [activatedGeneration, setActivatedGeneration] = useState<number>();

  const recover = async (action: ControlLogRecoveryAction) => {
    if (session === undefined || busy) return;
    setBusy(true);
    setFailed(false);
    try {
      const result = await bridge.recoverControlLog(workspaceId, session.id, action);
      if ("Preview" in result) {
        setPreview(result.Preview);
        setActivatedGeneration(undefined);
      } else {
        setActivatedGeneration(result.Activated.command_generation);
        setPreview(undefined);
      }
    } catch {
      // Preserve the exact preview when confirmation fails: a retry must resume this operation.
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  return <section className="support-checks" aria-labelledby="control-log-recovery-title" aria-busy={busy}>
    <div className="support-section-heading"><div>
      <h2 id="control-log-recovery-title">{t("controlLogRecoveryTitle")}</h2>
      <p>{t("controlLogRecoveryDetail")}</p>
    </div></div>
    <p>{session === undefined ? t("controlLogRecoverySelectSession") : t("controlLogRecoverySession", { session: session.label ?? session.id })}</p>
    {preview === undefined ? null : <>
      <dl className="control-log-recovery-facts">
        <div><dt>{t("controlLogRecoveryOldGeneration")}</dt><dd>{preview.authority.request.from_generation}</dd></div>
        <div><dt>{t("controlLogRecoveryBytes")}</dt><dd>{preview.authority.old_byte_length}</dd></div>
        <div><dt>{t("controlLogRecoveryDigest")}</dt><dd><code>{preview.authority.old_content_digest}</code></dd></div>
        <div><dt>{t("controlLogRecoveryNewGeneration")}</dt><dd>{preview.authority.request.successor_generation}</dd></div>
        <div><dt>{t("controlLogRecoveryPrefix")}</dt><dd>{t("controlLogRecoveryPrefixValue", { records: preview.impact.verified_record_count, bytes: preview.impact.verified_prefix_bytes })}</dd></div>
        <div><dt>{t("controlLogRecoveryKnownCommands")}</dt><dd>{preview.impact.known_command_count}</dd></div>
        <div><dt>{t("controlLogRecoveryScopes")}</dt><dd>{preview.impact.affected_scope_count}</dd></div>
        <div><dt>{t("controlLogRecoveryUnresolved")}</dt><dd>{preview.impact.known_unresolved_count}</dd></div>
      </dl>
      <p>{preview.impact.tail_command_count_unknown
        ? t("controlLogRecoveryUnknownTail", { bytes: preview.impact.unparsed_tail_bytes })
        : t("controlLogRecoveryNoUnknownTail")}</p>
      {preview.impact.affected_scopes.length === 0 ? null : <ul aria-label={t("controlLogRecoveryScopes")}>
        {preview.impact.affected_scopes.map((scope) => <li key={scope.scope_digest}>{scope.session_id ?? scope.workspace_id ?? scope.scope_digest}</li>)}
      </ul>}
      {preview.impact.scopes_truncated ? <p>{t("controlLogRecoveryScopesTruncated")}</p> : null}
      {preview.impact.unresolved_commands.length === 0 ? null : <ul className="control-log-recovery-commands" aria-label={t("controlLogRecoveryUnresolved")}>
        {preview.impact.unresolved_commands.map((command) => <li key={command.key_digest}><code>{command.command_id}</code> · {command.command_kind} · {command.phase}<br /><code>{command.key_digest}</code></li>)}
      </ul>}
      {preview.impact.commands_truncated ? <p>{t("controlLogRecoveryCommandsTruncated")}</p> : null}
      <p>{t("controlLogRecoveryConfirmation")}</p>
    </>}
    {failed ? <p role="alert">{t("controlLogRecoveryFailed")}</p> : null}
    {activatedGeneration === undefined ? null : <p role="status">{t("controlLogRecoveryActivated", { generation: activatedGeneration })}</p>}
    <div className="support-header-actions">
      <Button type="button" disabled={busy || session === undefined} onClick={() => void recover("Preview")}>
        {busy ? t("controlLogRecoveryWorking") : t("controlLogRecoveryPreview")}
      </Button>
      {preview === undefined ? null : <Button type="button" variant="primary" disabled={busy}
        onClick={() => void recover({ SealAndRotate: { preview } })}>
        {t("controlLogRecoveryConfirm")}
      </Button>}
    </div>
  </section>;
}
