import { useEffect, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type { ProviderSetupSaveResult } from "../../types";
import { Button, TextField } from "../../ui/primitives";

export function ProviderCredentialForm({ bridge, workspaceId, connectionId, connectionLabel, active, onSaved, onCancel, onOpenSupport }: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly connectionId: string;
  readonly connectionLabel: string;
  readonly active: boolean;
  readonly onSaved: (result: ProviderSetupSaveResult) => void;
  readonly onCancel: () => void;
  readonly onOpenSupport?: () => void;
}) {
  const { t } = useLocale();
  const [apiKey, setApiKey] = useState("");
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState(false);
  const [savedReloadFailed, setSavedReloadFailed] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const mounted = useRef(false);
  const busy = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useEffect(() => {
    if (active) input.current?.focus({ preventScroll: true });
  }, [active]);

  const save = async () => {
    if (apiKey.trim() === "" || busy.current) return;
    busy.current = true;
    setSaving(true);
    setFailed(false);
    try {
      const result = await bridge.updateProviderConnectionCredential(workspaceId, { connectionId, apiKey: apiKey.trim() });
      if (!mounted.current) return;
      setApiKey("");
      onSaved(result);
    } catch (error) {
      if (mounted.current) {
        if (typeof error === "object" && error !== null && "code" in error && error.code === "credential_saved_workspace_reload_failed") {
          setApiKey("");
          setSavedReloadFailed(true);
        } else setFailed(true);
      }
    } finally {
      busy.current = false;
      if (mounted.current) setSaving(false);
    }
  };

  return <form className="provider-credential-form" aria-label={t("updateConnectionCredential", { name: connectionLabel })}
    onSubmit={(event) => { event.preventDefault(); void save(); }}>
    {savedReloadFailed ? <>
      <p role="alert">{t("credentialSavedReloadFailed")}</p>
      <div className="provider-setup-actions">
        <Button type="button" variant="quiet" onClick={onCancel}>{t("dismiss")}</Button>
        {onOpenSupport === undefined ? null : <Button type="button" onClick={onOpenSupport}>{t("openSupport")}</Button>}
      </div>
    </> : <>
    <TextField ref={input} type="password" autoComplete="off" spellCheck={false} label={t("apiKey")}
      description={t("updateCredentialDetail")} value={apiKey} disabled={saving}
      onChange={(event) => setApiKey(event.target.value)} />
    {failed ? <p role="alert">{t("updateCredentialFailed")}</p> : null}
    <div className="provider-setup-actions">
      <Button type="button" variant="quiet" disabled={saving} onClick={onCancel}>{t("cancel")}</Button>
      <Button type="submit" busy={saving} disabled={apiKey.trim() === ""}>{t("save")}</Button>
    </div>
    </>}
  </form>;
}
