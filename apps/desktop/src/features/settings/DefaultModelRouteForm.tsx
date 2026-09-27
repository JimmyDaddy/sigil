import { useEffect, useId, useState } from "react";

import { useLocale } from "../../i18n";
import type { ProviderConnectionInventory, ProviderModelRef } from "../../types";
import { Button, Select, TextField } from "../../ui/primitives";

/** Edits a configured route without depending on a conversation or its runtime context. */
export function DefaultModelRouteForm({
  inventory,
  defaultModel,
  saving,
  onSave,
}: {
  readonly inventory: ProviderConnectionInventory;
  readonly defaultModel?: ProviderModelRef;
  readonly saving: boolean;
  readonly onSave: (model: ProviderModelRef) => Promise<void>;
}) {
  const { t } = useLocale();
  const suggestionsId = useId();
  const [connectionId, setConnectionId] = useState(defaultModel?.connectionId ?? "");
  const [modelId, setModelId] = useState(defaultModel?.modelId ?? "");
  const connection = inventory.connections.find((candidate) => candidate.id === connectionId);
  const knownModels = Object.keys(connection?.modelContextWindows ?? {});

  useEffect(() => {
    setConnectionId(defaultModel?.connectionId ?? "");
    setModelId(defaultModel?.modelId ?? "");
  }, [defaultModel?.connectionId, defaultModel?.modelId]);

  return (
    <form className="settings-model-controls" aria-label={t("defaultModel")} onSubmit={(event) => {
      event.preventDefault();
      if (saving || connection === undefined || modelId.trim() === "") return;
      void onSave({ connectionId: connection.id, modelId: modelId.trim() });
    }}>
      <Select
        label={t("configuredConnection")}
        description={t("defaultModelWithoutSessionDetail")}
        value={connectionId}
        disabled={saving}
        onChange={(event) => {
          const next = inventory.connections.find((candidate) => candidate.id === event.currentTarget.value);
          setConnectionId(event.currentTarget.value);
          setModelId(next?.defaultModel?.modelId ?? "");
        }}
      >
        <option value="" disabled>{t("configuredConnection")}</option>
        {inventory.connections.map((candidate) => (
          <option key={candidate.id} value={candidate.id}>
            {candidate.providerLabel} · {candidate.label}
          </option>
        ))}
      </Select>
      <TextField
        label={t("modelId")}
        description={t("enterModelManuallyDetail")}
        value={modelId}
        list={suggestionsId}
        disabled={saving}
        onChange={(event) => setModelId(event.currentTarget.value)}
      />
      <datalist id={suggestionsId}>
        {knownModels.map((model) => <option key={model} value={model} />)}
      </datalist>
      <Button type="submit" variant="secondary" disabled={saving || connection === undefined || modelId.trim() === ""}>
        {saving ? t("savingProvider") : t("saveDefaultModel")}
      </Button>
    </form>
  );
}
