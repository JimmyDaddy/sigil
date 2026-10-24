import { useRef, useState, type RefObject } from "react";

import { useLocale } from "../../i18n";
import type { ProviderConnection, ProviderModelRef } from "../../types";
import { Button, Dialog, Select, TextField } from "../../ui/primitives";
import { providerConnectionLabel } from "../settings/providerPresentation";
import { configuredModelRouteIssue } from "./modelRouteSelection";

export function CustomModelDialog({ connections, currentModel, returnFocusRef, onChoose, onCancel }: {
  readonly connections: readonly ProviderConnection[];
  readonly currentModel?: ProviderModelRef;
  readonly returnFocusRef: RefObject<HTMLSelectElement | null>;
  readonly onChoose: (model: ProviderModelRef) => void;
  readonly onCancel: () => void;
}) {
  const { t } = useLocale();
  const [connectionId, setConnectionId] = useState(currentModel?.connectionId ?? connections[0]?.id ?? "");
  const [modelId, setModelId] = useState(currentModel?.modelId ?? "");
  const connectionRef = useRef<HTMLSelectElement>(null);
  const composing = useRef(false);
  const compositionEndedAt = useRef<number | undefined>(undefined);
  const model = { connectionId, modelId: modelId.trim() };
  const issue = configuredModelRouteIssue(model, connections);

  return <Dialog open title={t("customModelTitle")} description={t("customModelScope")}
    initialFocusRef={connectionRef} returnFocusRef={returnFocusRef}
    onOpenChange={(open) => { if (!open) onCancel(); }}>
    <form className="custom-model-form" noValidate
      onCompositionStart={() => { composing.current = true; compositionEndedAt.current = undefined; }}
      onCompositionEnd={() => { composing.current = false; compositionEndedAt.current = Date.now(); }}
      onKeyUp={() => { compositionEndedAt.current = undefined; }}
      onPointerDown={() => { compositionEndedAt.current = undefined; }}
      onKeyDown={(event) => {
        const ended = compositionEndedAt.current;
        if ((event.key === "Enter" || event.key === "Escape")
          && (composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229
            || ended !== undefined && Date.now() - ended < 100)) {
          event.preventDefault();
          event.stopPropagation();
        }
      }}
      onSubmit={(event) => {
        event.preventDefault();
        event.stopPropagation();
        if (composing.current || configuredModelRouteIssue(model, connections) !== undefined) return;
        onChoose(model);
      }}>
      <Select ref={connectionRef} label={t("configuredConnection")} value={connectionId}
        error={issue === "customModelConnectionUnavailable" ? t(issue) : undefined}
        onChange={(event) => setConnectionId(event.currentTarget.value)}>
        {connections.some((connection) => connection.id === connectionId) ? null
          : <option value={connectionId} disabled>{t("configuredConnection")}</option>}
        {connections.map((connection) => <option key={connection.id} value={connection.id}>
          {providerConnectionLabel(connection)}
        </option>)}
      </Select>
      <TextField label={t("modelId")} value={modelId} autoComplete="off" spellCheck={false}
        error={issue !== undefined && issue !== "customModelConnectionUnavailable" ? t(issue) : undefined}
        onChange={(event) => setModelId(event.currentTarget.value)} />
      <div className="custom-model-actions">
        <Button type="button" variant="quiet" onClick={onCancel}>{t("cancel")}</Button>
        <Button type="submit" variant="primary" disabled={issue !== undefined}>{t("useModel")}</Button>
      </div>
    </form>
  </Dialog>;
}
