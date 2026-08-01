import { useState } from "react";

import type { DesktopBridge } from "../../bridge";
import type { ThemePreference } from "../../appearance/contract";
import { useAppearance } from "../../appearance/ThemeProvider";
import { type Locale, useLocale } from "../../i18n";
import { readReopenLastWorkspace, writeReopenLastWorkspace } from "../../preferences";
import { modelOptionIsSelectable } from "../../types";
import type {
  ProviderConnectionInventory,
  ProviderConnectionReadiness,
  ProviderCredentialSource,
  ProviderModelRef,
  RunContext,
} from "../../types";
import { Icon } from "../../ui/icons";
import { useNotifications } from "../../ui/feedback";
import { Button, Checkbox, Select } from "../../ui/primitives";
import { ApplicationPage } from "../navigation/ApplicationPage";
import { ProviderSetup } from "./ProviderSetup";
import { DesktopUpdateCard } from "./DesktopUpdateCard";

const themeOptions: readonly ThemePreference[] = [
  "system",
  "sigil_light",
  "sigil_dark",
  "solarized_light",
  "solarized_dark",
  "gruvbox_dark",
  "nord",
  "high_contrast_dark",
];

export function SettingsPage({
  bridge,
  supportAvailable,
  workspaceId,
  isWorkspaceActive,
  providerInventory,
  onProviderInventoryChange,
  modelContext,
  defaultModel,
  onDefaultModelChange,
  onBack,
  onOpenSupport,
}: {
  readonly bridge: DesktopBridge;
  readonly supportAvailable: boolean;
  readonly workspaceId?: string;
  readonly isWorkspaceActive: () => boolean;
  readonly providerInventory?: ProviderConnectionInventory;
  readonly onProviderInventoryChange: (inventory: ProviderConnectionInventory) => boolean;
  readonly modelContext?: RunContext;
  readonly defaultModel?: ProviderModelRef;
  readonly onDefaultModelChange: (modelRef?: ProviderModelRef) => void;
  readonly onBack: () => void;
  readonly onOpenSupport: () => void;
}) {
  const appearance = useAppearance();
  const { locale, setLocale, t } = useLocale();
  const { notify } = useNotifications();
  const [reopenLastWorkspace, setReopenLastWorkspace] = useState(readReopenLastWorkspace);
  const [providerSetupOpen, setProviderSetupOpen] = useState(false);
  const [providerReloading, setProviderReloading] = useState(false);
  const [defaultModelSaving, setDefaultModelSaving] = useState(false);
  const effectiveDefaultModel = defaultModel ?? providerInventory?.defaultModel;
  const defaultModelConnection = providerInventory?.connections.find(
    (connection) => connection.id === (effectiveDefaultModel?.connectionId ?? modelContext?.modelRef.connectionId),
  );

  const updateStartup = (enabled: boolean) => {
    if (!writeReopenLastWorkspace(enabled)) {
      notify({ tone: "error", message: t("settingsSaveFailed") });
      return;
    }
    setReopenLastWorkspace(enabled);
  };

  const modelRefKey = (modelRef: ProviderModelRef) =>
    `${modelRef.connectionId}/${modelRef.modelId}`;
  const modelRefLabel = (modelRef: ProviderModelRef) => {
    const option = modelContext?.modelOptions.find(
      (candidate) => modelRefKey(candidate.modelRef) === modelRefKey(modelRef),
    );
    if (option === undefined || option.displayName === option.modelName) return modelRef.modelId;
    return `${option.displayName} · ${option.modelName}`;
  };
  const updateDefaultModel = async (selection: string) => {
    const selected = modelContext?.modelOptions.find(
      (option) => modelRefKey(option.modelRef) === selection,
    );
    const connection = providerInventory?.connections.find(
      (candidate) => candidate.id === selected?.modelRef.connectionId,
    );
    if (
      workspaceId === undefined
      || selected === undefined
      || !modelOptionIsSelectable(selected)
      || connection === undefined
      || !["ready", "unverified"].includes(connection.readiness)
    ) {
      notify({ tone: "error", message: t("settingsSaveFailed") });
      return;
    }
    setDefaultModelSaving(true);
    try {
      const result = await bridge.saveProviderDefaultModel(workspaceId, selected.modelRef);
      if (!onProviderInventoryChange(result.inventory)) return;
      onDefaultModelChange(result.defaultModel);
      notify({
        tone: result.saveWarning ? "warning" : "success",
        message: result.saveWarning ? t("defaultModelSaveWarning") : t("defaultModelSaved"),
      });
    } catch {
      if (isWorkspaceActive()) {
        notify({ tone: "error", message: t("settingsSaveFailed") });
      }
    } finally {
      if (isWorkspaceActive()) setDefaultModelSaving(false);
    }
  };
  const reloadProviderConfiguration = async () => {
    if (workspaceId === undefined) return;
    setProviderReloading(true);
    try {
      onProviderInventoryChange(await bridge.providerConnections(workspaceId));
    } catch {
      if (isWorkspaceActive()) {
        notify({ tone: "error", message: t("providerConnectionsUnavailable") });
      }
    } finally {
      if (isWorkspaceActive()) {
        setProviderReloading(false);
      }
    }
  };

  return (
    <ApplicationPage
      className="settings-page"
      eyebrow={t("applicationPreferences")}
      title={t("settings")}
      detail={t("settingsDetail")}
      navigation={{ label: t("backToConversation"), onBack }}
    >

      <div className="settings-sections">
        <section className="settings-section settings-provider" aria-labelledby="settings-provider">
          <div className="settings-section-heading">
            <Icon name="model" />
            <div>
              <h2 id="settings-provider">{t("providerConnections")}</h2>
              <p>{t("providerConnectionsDetail")}</p>
            </div>
          </div>
          <div className="provider-connection-settings">
            {workspaceId === undefined ? (
              <p className="settings-control-unavailable">{t("providerWorkspaceRequired")}</p>
            ) : providerInventory === undefined ? (
              <p className="settings-control-unavailable">{t("loadingProviderConnections")}</p>
            ) : (
              <>
                {providerInventory.connections.length === 0 ? (
                  <p className="settings-control-unavailable">{t("noProviderConnections")}</p>
                ) : (
                  <ul className="provider-connection-list">
                    {providerInventory.connections.map((connection) => {
                      const isDefaultConnection = effectiveDefaultModel?.connectionId === connection.id;
                      return (
                        <li key={connection.id}>
                          <div>
                            <strong>{connection.label}</strong>
                            <span>{connection.providerLabel} · {connection.protocolLabel}</span>
                            <span>
                              {providerCredentialSourceLabel(connection.credentialSource, t)}
                              {" · "}
                              {connection.endpointDisplay}
                            </span>
                            <span className="provider-connection-model">
                              {isDefaultConnection && effectiveDefaultModel !== undefined
                                ? t("providerConnectionDefaultModel", {
                                  model: modelRefLabel(effectiveDefaultModel),
                                })
                                : t("providerConnectionNotDefault")}
                            </span>
                          </div>
                          <small data-readiness={connection.readiness}>
                            {providerReadinessLabel(connection.readiness, t)}
                          </small>
                        </li>
                      );
                    })}
                  </ul>
                )}
                {providerInventory.configMode === "v2" ? (
                  <Button
                    type="button"
                    variant="secondary"
                    onClick={() => setProviderSetupOpen(true)}
                  >
                    {t("addProviderConnection")}
                  </Button>
                ) : (
                  <div className="provider-setup-error" role="alert">
                    <p>{t("providerConfigInvalidDetail")}</p>
                    <div className="provider-setup-actions">
                      <Button
                        type="button"
                        variant="primary"
                        onClick={() => setProviderSetupOpen(true)}
                      >
                        {t("replaceProviderConfig")}
                      </Button>
                      <Button
                        type="button"
                        variant="secondary"
                        disabled={providerReloading}
                        onClick={() => void reloadProviderConfiguration()}
                      >
                        {providerReloading ? t("loading") : t("recheckProviderConfig")}
                      </Button>
                      <Button type="button" variant="quiet" onClick={onOpenSupport}>
                        {t("openSupport")}
                      </Button>
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
          {providerSetupOpen
            && workspaceId !== undefined
            && providerInventory !== undefined ? (
              <div className="settings-provider-setup">
                <ProviderSetup
                  bridge={bridge}
                  workspaceId={workspaceId}
                  inventory={providerInventory}
                  mode={providerInventory.configMode === "invalid" ? "repair" : "settings"}
                  onCancel={() => setProviderSetupOpen(false)}
                  onSaved={(inventory) => {
                    if (!onProviderInventoryChange(inventory)) return;
                    setProviderSetupOpen(false);
                    notify({ tone: "success", message: t("providerSetupSaved") });
                  }}
                />
              </div>
            ) : null}
        </section>

        <section className="settings-section" aria-labelledby="settings-model">
          <div className="settings-section-heading">
            <Icon name="model" />
            <div>
              <h2 id="settings-model">{t("defaultModel")}</h2>
              <p>{t("defaultModelDetail")}</p>
            </div>
          </div>
          {modelContext === undefined ? (
            <p className="settings-control-unavailable">{t("defaultModelUnavailable")}</p>
          ) : (
            <Select
              label={t("defaultModel")}
              description={t("defaultModelProvider", {
                connection: defaultModelConnection?.label ?? modelContext.modelRef.connectionId,
                provider: defaultModelConnection?.providerLabel ?? modelContext.providerName,
              })}
              value={
                effectiveDefaultModel === undefined ? "" : modelRefKey(effectiveDefaultModel)
              }
              disabled={defaultModelSaving}
              onChange={(event) => void updateDefaultModel(event.currentTarget.value)}
            >
              {modelContext.modelOptions.map((option) => (
                <option
                  key={modelRefKey(option.modelRef)}
                  value={modelRefKey(option.modelRef)}
                  disabled={
                    !modelOptionIsSelectable(option)
                    || !providerInventory?.connections.some(
                      (connection) => connection.id === option.modelRef.connectionId
                        && ["ready", "unverified"].includes(connection.readiness),
                    )
                  }
                >
                  {providerInventory?.connections.find(
                    (connection) => connection.id === option.modelRef.connectionId,
                  )?.providerLabel ?? option.modelRef.connectionId}
                  {" · "}
                  {providerInventory?.connections.find(
                    (connection) => connection.id === option.modelRef.connectionId,
                  )?.label ?? option.modelRef.connectionId}
                  {" · "}
                  {option.displayName === option.modelName
                    ? option.modelName
                    : `${option.displayName} · ${option.modelName}`}
                  {option.availability === "configured_unavailable"
                    ? ` · ${t("unavailable")}`
                    : ""}
                </option>
              ))}
            </Select>
          )}
        </section>

        <section className="settings-section" aria-labelledby="settings-appearance">
          <div className="settings-section-heading">
            <Icon name="sun" />
            <div>
              <h2 id="settings-appearance">{t("appearance")}</h2>
              <p>{t("appearanceDetail")}</p>
            </div>
          </div>
          <div className="theme-option-grid" role="group" aria-label={t("appearance")}>
            {themeOptions.map((option) => (
              <Button
                key={option}
                type="button"
                variant="secondary"
                className="theme-option"
                data-theme-option={option}
                aria-label={themeName(option, t)}
                aria-pressed={appearance.preference === option}
                disabled={appearance.status === "saving"}
                onClick={() => void appearance.setPreference(option)}
              >
                <span className="theme-option-content">
                  <span className="theme-option-preview" data-theme-preview={option} aria-hidden="true">
                    <i />
                    <i />
                    <i />
                  </span>
                  <span className="theme-option-copy">
                    <strong>{themeName(option, t)}</strong>
                    <small>{themeDescription(option, t)}</small>
                  </span>
                </span>
              </Button>
            ))}
          </div>
          {appearance.error === undefined ? null : (
            <div className="settings-inline-error">
              <span>{appearance.error}</span>
              <Button type="button" onClick={() => void appearance.retry()}>{t("retry")}</Button>
            </div>
          )}
        </section>

        <section className="settings-section" aria-labelledby="settings-language">
          <div className="settings-section-heading">
            <Icon name="language" />
            <div>
              <h2 id="settings-language">{t("languageSetting")}</h2>
              <p>{t("languageSettingDetail")}</p>
            </div>
          </div>
          <div className="settings-choice-group" role="group" aria-label={t("languageSetting")}>
            {(["en", "zh-CN"] as Locale[]).map((value) => (
              <Button
                key={value}
                type="button"
                variant={locale === value ? "primary" : "secondary"}
                aria-pressed={locale === value}
                onClick={() => setLocale(value)}
              >
                {value === "en" ? "English" : "简体中文"}
              </Button>
            ))}
          </div>
        </section>

        <section className="settings-section" aria-labelledby="settings-startup">
          <div className="settings-section-heading">
            <Icon name="history" />
            <div>
              <h2 id="settings-startup">{t("startup")}</h2>
              <p>{t("startupDetail")}</p>
            </div>
          </div>
          <Checkbox
            label={t("reopenLastWorkspace")}
            description={t("reopenLastWorkspaceDetail")}
            checked={reopenLastWorkspace}
            onChange={(event) => updateStartup(event.currentTarget.checked)}
          />
        </section>

        <DesktopUpdateCard bridge={bridge} />

        <section className="settings-section settings-boundary" aria-labelledby="settings-runtime">
          <div className="settings-section-heading">
            <Icon name="shield" />
            <div>
              <h2 id="settings-runtime">{t("runtimeControls")}</h2>
              <p>{t("runtimeControlsDetail")}</p>
            </div>
          </div>
          <div className="settings-choice-group">
            <Button
              type="button"
              variant="secondary"
              leadingIcon={<Icon name="shield" />}
              disabled={!supportAvailable}
              onClick={onOpenSupport}
            >
              {t("openSupport")}
            </Button>
          </div>
        </section>
      </div>
    </ApplicationPage>
  );
}

function providerReadinessLabel(
  readiness: ProviderConnectionReadiness,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (readiness) {
    case "ready": return t("providerReadiness_ready");
    case "needs_credential": return t("providerReadiness_needs_credential");
    case "credential_unavailable": return t("providerReadiness_credential_unavailable");
    case "needs_model": return t("providerReadiness_needs_model");
    case "unverified": return t("providerReadiness_unverified");
    case "invalid": return t("providerReadiness_invalid");
  }
}

function providerCredentialSourceLabel(
  source: ProviderCredentialSource,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (source) {
    case "environment": return t("environmentVariable");
    case "stored": return t("secureStore");
    case "none": return t("noAuthentication");
  }
}

function themeName(preference: ThemePreference, t: ReturnType<typeof useLocale>["t"]): string {
  switch (preference) {
    case "system": return t("systemTheme");
    case "sigil_light": return t("sigilLightTheme");
    case "sigil_dark": return t("sigilDarkTheme");
    case "solarized_light": return t("solarizedLightTheme");
    case "solarized_dark": return t("solarizedDarkTheme");
    case "gruvbox_dark": return t("gruvboxDarkTheme");
    case "nord": return t("nordTheme");
    case "high_contrast_dark": return t("highContrastDarkTheme");
  }
}

function themeDescription(
  preference: ThemePreference,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (preference) {
    case "system": return t("systemThemeDetail");
    case "sigil_light": return t("sigilLightThemeDetail");
    case "sigil_dark": return t("sigilDarkThemeDetail");
    case "solarized_light":
    case "solarized_dark": return t("solarizedThemeDetail");
    case "gruvbox_dark": return t("gruvboxThemeDetail");
    case "nord": return t("nordThemeDetail");
    case "high_contrast_dark": return t("highContrastThemeDetail");
  }
}
