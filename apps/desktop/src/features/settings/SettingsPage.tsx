import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import type { ThemePreference } from "../../appearance/contract";
import { useAppearance } from "../../appearance/ThemeProvider";
import { type Locale, useLocale } from "../../i18n";
import { readReopenLastWorkspace, writeReopenLastWorkspace } from "../../preferences";
import { modelOptionIsSelectable } from "../../types";
import type {
  ModelOption,
  ProviderConnectionInventory,
  ProviderConnectionReadiness,
  ProviderCredentialSource,
  ProviderModelRef,
  RunContext,
} from "../../types";
import { Icon } from "../../ui/icons";
import { useNotifications } from "../../ui/feedback";
import { Button, Checkbox, Menu, MenuItem, Select, TextField } from "../../ui/primitives";
import { ApplicationPage } from "../navigation/ApplicationPage";
import { ProviderSetup } from "./ProviderSetup";
import { ProviderCredentialForm } from "./ProviderCredentialForm";
import { PluginReviewCard } from "./PluginReviewCard";
import { McpImportCard } from "./McpImportCard";
import { DesktopUpdateCard } from "./DesktopUpdateCard";
import { DefaultModelRouteForm } from "./DefaultModelRouteForm";
import { type ContextWindowDrafts, useContextWindowDrafts } from "./useContextWindowDrafts";
import { settingsCategories, type SettingsCategory, type SettingsNavigationState } from "./settingsNavigation";
import {
  connectionMatchesProvider,
  providerConnectionDetail,
  providerConnectionLabel,
} from "./providerPresentation";

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
  sessionId,
  isWorkspaceActive,
  providerInventory,
  providerInventoryState,
  onProviderInventoryChange,
  onProviderConfigurationReloaded,
  modelContext,
  defaultModel,
  contextWindowDrafts,
  onDefaultModelChange,
  onBack,
  onOpenSupport,
  navigationState,
  onNavigationStateChange,
}: {
  readonly bridge: DesktopBridge;
  readonly supportAvailable: boolean;
  readonly workspaceId?: string;
  readonly sessionId?: string;
  readonly isWorkspaceActive: () => boolean;
  readonly providerInventory?: ProviderConnectionInventory;
  readonly providerInventoryState?: "idle" | "loading" | "ready" | "error";
  readonly onProviderInventoryChange: (inventory: ProviderConnectionInventory) => boolean;
  readonly onProviderConfigurationReloaded: (workspaceId: string) => Promise<void>;
  readonly modelContext?: RunContext;
  readonly defaultModel?: ProviderModelRef;
  readonly contextWindowDrafts?: ContextWindowDrafts;
  readonly onDefaultModelChange: (modelRef?: ProviderModelRef) => void;
  readonly onBack: () => void;
  readonly onOpenSupport: () => void;
  readonly navigationState?: SettingsNavigationState;
  readonly onNavigationStateChange?: (state: SettingsNavigationState) => void;
}) {
  const appearance = useAppearance();
  const { locale, setLocale, t } = useLocale();
  const { notify } = useNotifications();
  const [reopenLastWorkspace, setReopenLastWorkspace] = useState(readReopenLastWorkspace);
  const [providerSetupOpen, setProviderSetupOpen] = useState(false);
  const [credentialConnectionId, setCredentialConnectionId] = useState<string>();
  const credentialTrigger = useRef<HTMLButtonElement | null>(null);
  const [category, setCategory] = useState<SettingsCategory>(navigationState?.category ?? "connections");
  const categoryId = useId();
  const sectionsRef = useRef<HTMLDivElement>(null);
  const navigationSnapshot = useRef<SettingsNavigationState>(navigationState ?? { category: "connections", scrollTop: 0 });
  const rememberNavigation = (next: Partial<SettingsNavigationState>) => {
    navigationSnapshot.current = { ...navigationSnapshot.current, ...next };
    onNavigationStateChange?.(navigationSnapshot.current);
  };
  useLayoutEffect(() => {
    const sections = sectionsRef.current;
    if (sections === null || navigationState === undefined) return;
    sections.scrollTop = navigationState.scrollTop;
    if (navigationState.focusKey === undefined) return;
    const entry = [...(sections.parentElement?.querySelectorAll<HTMLElement>("[data-settings-focus-key]") ?? [])]
      .find((candidate) => candidate.dataset.settingsFocusKey === navigationState.focusKey);
    const trigger = entry?.matches("button") ? entry : entry?.querySelector<HTMLButtonElement>("button");
    if (trigger?.isConnected && !trigger.matches(":disabled") && trigger.closest("[hidden], [inert]") === null) {
      trigger.focus({ preventScroll: true });
    }
  }, []);
  const providerSetupTrigger = useRef<HTMLButtonElement | null>(null);
  const categoryPanel = (value: SettingsCategory) => ({
    id: `${categoryId}-${value}-panel`,
    role: "region",
    "aria-labelledby": `${categoryId}-${value}`,
    hidden: category !== value,
  });
  const closeProviderSetup = () => {
    setProviderSetupOpen(false);
    window.requestAnimationFrame(() => {
      const target = providerSetupTrigger.current;
      if (target?.isConnected && target.closest("[hidden], [inert]") === null) {
        target.focus({ preventScroll: true });
        target.scrollIntoView?.({ block: "nearest" });
      }
    });
  };
  const closeCredentialForm = () => {
    setCredentialConnectionId(undefined);
    window.requestAnimationFrame(() => {
      const target = credentialTrigger.current;
      if (target?.isConnected && target.closest("[hidden], [inert]") === null) target.focus({ preventScroll: true });
    });
  };
  const [providerReloading, setProviderReloading] = useState(false);
  const [providerLoadFailed, setProviderLoadFailed] = useState(false);
  const [defaultModelSaving, setDefaultModelSaving] = useState(false);
  const effectiveDefaultModel = defaultModel ?? providerInventory?.defaultModel;
  const defaultModelConnection = providerInventory?.connections.find(
    (connection) => connection.id === (effectiveDefaultModel?.connectionId ?? modelContext?.modelRef.connectionId),
  );
  const defaultConnectionIdentity = {
    label: defaultModelConnection?.label ?? modelContext?.modelRef.connectionId ?? "",
    providerLabel: defaultModelConnection?.providerLabel ?? modelContext?.providerName ?? "",
  };
  const localContextWindowDrafts = useContextWindowDrafts();
  const drafts = contextWindowDrafts ?? localContextWindowDrafts;
  const contextWindowScope = workspaceId === undefined || effectiveDefaultModel === undefined
    ? undefined
    : JSON.stringify([workspaceId, effectiveDefaultModel.connectionId, effectiveDefaultModel.modelId]);
  const configuredContextWindow = effectiveDefaultModel === undefined
    ? ""
    : defaultModelConnection?.modelContextWindows?.[effectiveDefaultModel.modelId]?.toString() ?? "";
  const contextWindowTokens = contextWindowScope === undefined
    ? configuredContextWindow
    : drafts.values[contextWindowScope] ?? configuredContextWindow;
  const contextWindowValue = contextWindowTokens.trim();
  const contextWindowDirty = contextWindowValue !== configuredContextWindow;
  const contextWindowValid = contextWindowValue === ""
    || (/^\d+$/.test(contextWindowValue)
      && Number.isSafeInteger(Number(contextWindowValue))
      && Number(contextWindowValue) > 0);

  useEffect(() => {
    if (contextWindowScope !== undefined && !contextWindowDirty) drafts.discard(contextWindowScope);
  }, [contextWindowDirty, contextWindowScope, drafts.discard]);

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
  const modelOptionLabel = (option: ModelOption) => {
    const connection = providerInventory?.connections.find(
      (candidate) => candidate.id === option.modelRef.connectionId,
    );
    const connectionLabel = providerConnectionLabel(connection ?? {
      label: option.modelRef.connectionId,
      providerLabel: option.modelRef.connectionId,
    });
    const modelLabel = option.displayName === option.modelName
      ? option.modelName
      : `${option.displayName} · ${option.modelName}`;
    return `${connectionLabel} · ${modelLabel}${option.availability === "configured_unavailable"
      ? ` · ${t("modelNotListed")}`
      : ""}`;
  };
  const updateDefaultModel = async (selection: string) => {
    const selected = modelContext?.modelOptions.find(
      (option) => modelRefKey(option.modelRef) === selection,
    );
    if (selected === undefined || !modelOptionIsSelectable(selected)) {
      notify({ tone: "error", message: t("settingsSaveFailed") });
      return;
    }
    await saveDefaultModel(selected.modelRef);
  };
  const saveDefaultModel = async (modelRef: ProviderModelRef) => {
    const connection = providerInventory?.connections.find(
      (candidate) => candidate.id === modelRef.connectionId,
    );
    if (
      workspaceId === undefined
      || connection === undefined
      || modelRef.modelId.trim() === ""
    ) {
      notify({ tone: "error", message: t("settingsSaveFailed") });
      return;
    }
    setDefaultModelSaving(true);
    try {
      const configuredContextWindow = connection.modelContextWindows?.[modelRef.modelId];
      const result = configuredContextWindow === undefined
        ? await bridge.saveProviderDefaultModel(workspaceId, modelRef)
        : await bridge.saveProviderDefaultModel(
          workspaceId,
          modelRef,
          configuredContextWindow,
        );
      if (!onProviderInventoryChange(result.inventory)) return;
      onDefaultModelChange(result.defaultModel);
      await onProviderConfigurationReloaded(workspaceId);
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
  const saveContextWindow = async () => {
    if (
      workspaceId === undefined
      || effectiveDefaultModel === undefined
      || contextWindowScope === undefined
      || !contextWindowValid
    ) return;
    setDefaultModelSaving(true);
    try {
      const result = await bridge.saveProviderDefaultModel(
        workspaceId,
        effectiveDefaultModel,
        contextWindowValue === "" ? undefined : Number(contextWindowValue),
      );
      if (!onProviderInventoryChange(result.inventory)) return;
      onDefaultModelChange(result.defaultModel);
      await onProviderConfigurationReloaded(workspaceId);
      drafts.discard(contextWindowScope, contextWindowTokens);
      notify({
        tone: result.saveWarning ? "warning" : "success",
        message: result.saveWarning ? t("defaultModelSaveWarning") : t("contextWindowSaved"),
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
    if (workspaceId === undefined || providerReloading) return;
    setProviderReloading(true);
    setProviderLoadFailed(false);
    try {
      onProviderInventoryChange(await bridge.providerConnections(workspaceId));
    } catch {
      if (isWorkspaceActive()) {
        setProviderLoadFailed(true);
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

      <div className="settings-workbench" onFocusCapture={(event) => {
        const focusKey = (event.target as HTMLElement).closest<HTMLElement>("[data-settings-focus-key]")?.dataset.settingsFocusKey;
        if (focusKey !== undefined) rememberNavigation({ focusKey });
      }}>
        <nav className="settings-navigation" aria-label={t("settingsCategories")}>
          {settingsCategories.map((value) => (
            <Button key={value} id={`${categoryId}-${value}`} type="button"
              data-settings-focus-key={`category:${value}`}
              variant={category === value ? "secondary" : "quiet"}
              aria-current={category === value ? "page" : undefined}
              aria-controls={`${categoryId}-${value}-panel`}
              onClick={() => {
                setCategory(value);
                rememberNavigation({ category: value, scrollTop: 0, focusKey: `category:${value}` });
                if (sectionsRef.current !== null) sectionsRef.current.scrollTop = 0;
              }}>
              {t(`settingsCategory_${value}`)}
            </Button>
          ))}
        </nav>
        <div className="settings-sections" ref={sectionsRef} onScroll={(event) => rememberNavigation({ scrollTop: event.currentTarget.scrollTop })}>
        <div className="settings-category" {...categoryPanel("connections")}>
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
                !providerReloading && (providerInventoryState === "error" || providerLoadFailed) ? (
                  <div className="provider-setup-error" role="alert">
                    <p>{t("providerConnectionsUnavailable")}</p>
                    <Button type="button" variant="secondary" onClick={() => void reloadProviderConfiguration()}>{t("retry")}</Button>
                  </div>
                ) : <p className="settings-control-unavailable" role="status">{t("loadingProviderConnections")}</p>
              ) : (
                <>
                  {providerInventory.connections.length === 0 ? (
                    <p className="settings-control-unavailable">{t("noProviderConnections")}</p>
                  ) : (
                    <ul className="provider-connection-list">
                      {providerInventory.connections.map((connection) => {
                        const isDefaultConnection = effectiveDefaultModel?.connectionId === connection.id;
                        const connectionDetail = providerConnectionDetail(connection);
                        return (
                          <li key={connection.id}>
                            <div>
                              <strong>{connection.label}</strong>
                              {connectionDetail === "" ? null : <span>{connectionDetail}</span>}
                              <span>
                                {providerCredentialSourceLabel(connection.credentialSource, t)}
                                {" · "}
                                {connection.endpointDisplay}
                              </span>
                              {!isDefaultConnection || effectiveDefaultModel === undefined ? null : <span className="provider-connection-model">
                                {t("providerConnectionDefaultModel", { model: modelRefLabel(effectiveDefaultModel) })}
                              </span>}
                            </div>
                            <div className="provider-connection-actions">
                            <small data-readiness={connection.readiness}>
                              {providerReadinessLabel(connection.readiness, t)}
                            </small>
                            {connection.credentialSource !== "none" && connection.readiness !== "invalid" ? (
                              <CredentialAction connectionId={connection.id} connectionLabel={connection.label.trim()}
                                credentialSource={connection.credentialSource} readiness={connection.readiness}
                                expanded={credentialConnectionId === connection.id}
                                onOpen={(trigger) => {
                                  credentialTrigger.current = trigger;
                                  setCredentialConnectionId(connection.id);
                                  setProviderSetupOpen(false);
                                }} />
                            ) : connection.readiness === "invalid" ? (
                              <Button type="button" variant="quiet" data-settings-focus-key={`connection-support:${connection.id}`} onClick={() => {
                                rememberNavigation({ focusKey: `connection-support:${connection.id}` });
                                onOpenSupport();
                              }}>{t("checkConnectionConfiguration")}</Button>
                            ) : null}
                            </div>
                            {credentialConnectionId === connection.id && workspaceId !== undefined ? (
                              <ProviderCredentialForm key={connection.id} bridge={bridge} workspaceId={workspaceId}
                                connectionId={connection.id} connectionLabel={connection.label.trim()} credentialSource={connection.credentialSource} active={category === "connections"}
                                onCancel={closeCredentialForm} onOpenSupport={() => {
                                  rememberNavigation({ focusKey: `credential:${connection.id}` });
                                  onOpenSupport();
                                }} onSaved={(result) => {
                                  if (!onProviderInventoryChange(result.inventory)) return;
                                  closeCredentialForm();
                                  void onProviderConfigurationReloaded(workspaceId).then(() => {
                                    if (isWorkspaceActive()) notify({ tone: result.saveWarning ? "warning" : "success", message: t(result.saveWarning ? "defaultModelSaveWarning" : "credentialUpdated") });
                                  }).catch(() => {
                                    if (isWorkspaceActive()) notify({ tone: "error", message: t("providerSetupRestartFailed") });
                                  });
                                }} />
                            ) : null}
                          </li>
                        );
                      })}
                    </ul>
                  )}
                  {providerInventory.configMode === "v2" ? (
                    <Button
                      type="button"
                      variant="secondary"
                      onClick={(event) => {
                        providerSetupTrigger.current = event.currentTarget;
                        setProviderSetupOpen(true);
                      }}
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
                          onClick={(event) => {
                            providerSetupTrigger.current = event.currentTarget;
                            setProviderSetupOpen(true);
                          }}
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
                        <Button type="button" variant="quiet" data-settings-focus-key="configuration-support" onClick={() => {
                          rememberNavigation({ focusKey: "configuration-support" });
                          onOpenSupport();
                        }}>
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
                    active={category === "connections"}
                    onCancel={closeProviderSetup}
                    onSaved={(inventory) => {
                      if (!onProviderInventoryChange(inventory)) return;
                      void onProviderConfigurationReloaded(workspaceId).then(() => {
                        setProviderSetupOpen(false);
                        notify({ tone: "success", message: t("providerSetupSaved") });
                      }).catch(() => {
                        notify({ tone: "error", message: t("providerSetupRestartFailed") });
                      });
                    }}
                  />
                </div>
              ) : null}
          </section>

          {providerInventory !== undefined
            && providerInventoryState !== "loading"
            && providerInventoryState !== "error" ? (
          <section className="settings-section" aria-labelledby="settings-model">
            <div className="settings-section-heading">
              <Icon name="model" />
              <div>
                <h2 id="settings-model">{t("defaultModel")}</h2>
                <p>{t("defaultModelDetail")}</p>
              </div>
            </div>
            <div className="settings-model-controls">
              {modelContext === undefined && providerInventory?.configMode === "v2"
                && providerInventory.connections.length > 0 ? (
                <DefaultModelRouteForm
                  key={workspaceId}
                  inventory={providerInventory}
                  defaultModel={effectiveDefaultModel}
                  saving={defaultModelSaving}
                  onSave={saveDefaultModel}
                />
              ) : modelContext === undefined ? (
                <p className="settings-control-unavailable">
                  {effectiveDefaultModel === undefined
                    ? t("defaultModelUnavailable")
                    : t("defaultModelPickerUnavailable")}
                </p>
              ) : (
                <Select
                  label={t("defaultModel")}
                  description={connectionMatchesProvider(defaultConnectionIdentity)
                    ? t("defaultModelConnection", { connection: defaultConnectionIdentity.label.trim() })
                    : t("defaultModelProvider", {
                      connection: defaultConnectionIdentity.label,
                      provider: defaultConnectionIdentity.providerLabel,
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
                          (connection) => connection.id === option.modelRef.connectionId,
                        )
                      }
                    >
                      {modelOptionLabel(option)}
                    </option>
                  ))}
                </Select>
              )}
              {effectiveDefaultModel === undefined ? null : (
                <div className="settings-context-window-control">
                  <TextField
                    type="number"
                    min={1}
                    inputMode="numeric"
                    label={t("contextWindow")}
                    description={t("contextWindowSettingsDetail", {
                      connection: defaultModelConnection?.label
                        ?? effectiveDefaultModel.connectionId,
                      model: effectiveDefaultModel.modelId,
                    })}
                    value={contextWindowTokens}
                    placeholder={t("automatic")}
                    error={contextWindowValid ? undefined : t("contextWindowInvalid")}
                    disabled={defaultModelSaving || contextWindowScope === undefined}
                    onChange={(event) => {
                      if (contextWindowScope !== undefined) {
                        drafts.update(contextWindowScope, event.currentTarget.value, configuredContextWindow);
                      }
                    }}
                  />
                  <div className="settings-context-window-actions">
                    {contextWindowDirty ? <span className="settings-draft-status" role="status">{t("settingsUnsavedChanges")}</span> : null}
                    {contextWindowDirty ? <Button type="button" variant="quiet" disabled={defaultModelSaving}
                      onClick={() => { if (contextWindowScope !== undefined) drafts.discard(contextWindowScope); }}>
                      {t("settingsDiscardChanges")}
                    </Button> : null}
                    <Button
                      type="button"
                      variant="secondary"
                      disabled={defaultModelSaving || !contextWindowValid || !contextWindowDirty}
                      onClick={() => void saveContextWindow()}
                    >
                      {defaultModelSaving ? t("savingProvider") : t("save")}
                    </Button>
                  </div>
                </div>
              )}
              </div>
          </section>
          ) : null}

        </div>
        <div className="settings-category" {...categoryPanel("preferences")}>
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

        </div>
        <div className="settings-category" {...categoryPanel("extensions")}>
          {workspaceId === undefined ? <p className="settings-control-unavailable">{t("settingsExtensionsWorkspaceRequired")}</p> : null}
          {workspaceId !== undefined && sessionId !== undefined && <PluginReviewCard key={`${workspaceId}:${sessionId}`} bridge={bridge} workspaceId={workspaceId} sessionId={sessionId} />}

          {workspaceId !== undefined && <McpImportCard key={workspaceId}
            bridge={bridge} workspaceId={workspaceId}
            onImported={async () => {
              if (isWorkspaceActive()) await onProviderConfigurationReloaded(workspaceId);
            }} />}

        </div>
        <div className="settings-category" {...categoryPanel("support")}>
          <DesktopUpdateCard bridge={bridge} />

          <section className="settings-section settings-boundary" aria-labelledby="settings-runtime">
            <div className="settings-section-heading">
              <Icon name="shield" />
              <div>
                <h2 id="settings-runtime">{t("supportDiagnostics")}</h2>
                <p>{t("settingsSupportDetail")}</p>
              </div>
            </div>
            <div className="settings-choice-group">
              <Button
                type="button"
                variant="secondary"
                leadingIcon={<Icon name="shield" />}
                disabled={!supportAvailable}
                data-settings-focus-key="open-support"
                onClick={() => {
                  rememberNavigation({ focusKey: "open-support" });
                  onOpenSupport();
                }}
              >
                {t("openSupport")}
              </Button>
            </div>
          </section>
        </div>
        </div>
      </div>
    </ApplicationPage>
  );
}

function CredentialAction({ connectionId, connectionLabel, credentialSource, readiness, expanded, onOpen }: {
  readonly connectionId: string;
  readonly connectionLabel: string;
  readonly credentialSource: ProviderCredentialSource;
  readonly readiness: ProviderConnectionReadiness;
  readonly expanded: boolean;
  readonly onOpen: (trigger: HTMLButtonElement) => void;
}) {
  const { t } = useLocale();
  const trigger = useRef<HTMLButtonElement>(null);
  const label = t(credentialSource === "environment" ? "useStoredCredential" : "updateCredential");
  const open = () => {
    const target = trigger.current;
    if (target?.isConnected && target.closest("[hidden], [inert]") === null) onOpen(target);
  };
  return <span className="provider-credential-action" data-settings-focus-key={`credential:${connectionId}`}>
    {readiness === "needs_credential" || readiness === "credential_unavailable" ? (
      <Button ref={trigger} type="button" variant="quiet"
        aria-label={t("updateConnectionCredential", { name: connectionLabel })}
        aria-expanded={expanded} onClick={open}>{label}</Button>
    ) : <Menu label={<Icon name="more" />} accessibleLabel={t("manageProviderConnection", { name: connectionLabel })} triggerRef={trigger}>
      <MenuItem onSelect={() => {
        // Let the menu finish returning focus before the inline editor takes it.
        window.requestAnimationFrame(() => window.requestAnimationFrame(open));
      }}>{label}</MenuItem>
    </Menu>}
  </span>;
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
