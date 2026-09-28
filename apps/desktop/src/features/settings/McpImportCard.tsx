import { useState } from "react";
import type { DesktopBridge } from "../../bridge";
import type { McpImportPreview } from "../../types";
import { useLocale } from "../../i18n";
import { Button, Checkbox } from "../../ui/primitives";

export function McpImportCard({ bridge, workspaceId, onImported }: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly onImported: () => Promise<void>;
}) {
  const { t } = useLocale();
  const [preview, setPreview] = useState<McpImportPreview>();
  const [selected, setSelected] = useState<number[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [saved, setSaved] = useState<string>();

  async function pick() {
    setBusy(true);
    setError(undefined);
    try {
      const next = await bridge.pickMcpImport(workspaceId);
      if (next !== null) {
        setPreview(next);
        setSelected(next.candidates.filter((entry) => entry.importable).map((entry) => entry.index));
        setSaved(undefined);
      }
    } catch {
      setError(t("mcpImportPickFailed"));
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    if (preview === undefined) return;
    setBusy(true);
    setError(undefined);
    try {
      const result = await bridge.applyMcpImport(workspaceId, preview.previewId, selected);
      setSaved(t("mcpImportSaved", { names: result.importedNames.join(", ") }));
      setPreview(undefined);
      setSelected([]);
      if (result.reloadRequired) setError(t("mcpImportReloadRequired"));
      else {
        try { await onImported(); } catch { setError(t("mcpImportReloadRequired")); }
      }
    } catch {
      setError(t("mcpImportFailed"));
    } finally {
      setBusy(false);
    }
  }

  return <section className="settings-section" aria-labelledby="settings-mcp-import">
    <h2 id="settings-mcp-import">{t("mcpImportTitle")}</h2>
    <p>{t("mcpImportDetail")}</p>
    <Button type="button" variant="secondary" disabled={busy} onClick={() => void pick()}>
      {busy ? t("loading") : t("mcpImportPick")}
    </Button>
    {preview !== undefined && <>
      {preview.rootFieldsIgnored && <p>{t("mcpImportIgnored")}</p>}
      {preview.candidates.length === 0 && <p>{t("mcpImportEmpty")}</p>}
      <ul className="provider-connection-list">
        {preview.candidates.map((entry) => <li key={entry.index}>
          <Checkbox label={entry.name} checked={selected.includes(entry.index)}
            disabled={busy || !entry.importable}
            onChange={(event) => { const checked = event.currentTarget.checked; setSelected((values) => checked
              ? [...values, entry.index] : values.filter((index) => index !== entry.index)); }} />
          <span>{entry.transport} {entry.description}</span>
          {!entry.importable && <p>{t("mcpImportInvalid")}</p>}
          {entry.issues.some((issue) => issue === "environment_values_not_imported" || issue === "header_values_not_imported")
            && <p>{t("mcpImportCredentials")}</p>}
          {entry.issues.includes("command_arguments_will_be_saved") && <p>{t("mcpImportArguments")}</p>}
          {entry.issues.includes("ignored_fields") && <p>{t("mcpImportIgnored")}</p>}
        </li>)}
      </ul>
      <Button type="button" variant="primary" disabled={busy || selected.length === 0} onClick={() => void save()}>
        {t("mcpImportSave")}
      </Button>
    </>}
    {error !== undefined && <p role="alert">{error}</p>}
    {saved !== undefined && <p role="status">{saved}</p>}
  </section>;
}
