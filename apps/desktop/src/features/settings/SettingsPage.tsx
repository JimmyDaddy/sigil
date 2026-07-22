import { useEffect, useRef, useState } from "react";

import type { ThemePreference } from "../../appearance/contract";
import { useAppearance } from "../../appearance/ThemeProvider";
import { type Locale, useLocale } from "../../i18n";
import { readReopenLastWorkspace, writeDefaultModel, writeReopenLastWorkspace } from "../../preferences";
import type { RunContext } from "../../types";
import { Icon, type IconName } from "../../ui/icons";
import { useNotifications } from "../../ui/feedback";
import { Button, Checkbox, Select } from "../../ui/primitives";
import { ApplicationPage } from "../navigation/ApplicationPage";

const themeOptions: ReadonlyArray<{ value: ThemePreference; icon: IconName }> = [
  { value: "system", icon: "appearance-auto" },
  { value: "light", icon: "sun" },
  { value: "dark", icon: "moon" },
];

export function SettingsPage({
  supportAvailable,
  workspaceId,
  modelContext,
  defaultModel,
  onDefaultModelChange,
  onBack,
  onOpenSupport,
}: {
  readonly supportAvailable: boolean;
  readonly workspaceId?: string;
  readonly modelContext?: RunContext;
  readonly defaultModel?: string;
  readonly onDefaultModelChange: (modelName?: string) => void;
  readonly onBack: () => void;
  readonly onOpenSupport: () => void;
}) {
  const appearance = useAppearance();
  const { locale, setLocale, t } = useLocale();
  const { dismiss, notify } = useNotifications();
  const [reopenLastWorkspace, setReopenLastWorkspace] = useState(readReopenLastWorkspace);
  const notifiedAppearanceError = useRef<string | undefined>(undefined);
  const appearanceNotificationId = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (appearance.error === undefined) {
      if (appearanceNotificationId.current !== undefined) dismiss(appearanceNotificationId.current);
      appearanceNotificationId.current = undefined;
      notifiedAppearanceError.current = undefined;
      return;
    }
    if (notifiedAppearanceError.current === appearance.error) return;
    notifiedAppearanceError.current = appearance.error;
    appearanceNotificationId.current = notify({ tone: "error", message: appearance.error });
  }, [appearance.error, dismiss, notify]);

  const updateStartup = (enabled: boolean) => {
    if (!writeReopenLastWorkspace(enabled)) {
      notify({ tone: "error", message: t("settingsSaveFailed") });
      return;
    }
    setReopenLastWorkspace(enabled);
  };

  const updateDefaultModel = (modelName: string) => {
    const preference = modelName === "" ? undefined : modelName;
    if (workspaceId === undefined || !writeDefaultModel(workspaceId, preference)) {
      notify({ tone: "error", message: t("settingsSaveFailed") });
      return;
    }
    onDefaultModelChange(preference);
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
              description={t("defaultModelProvider", { provider: modelContext.providerName })}
              value={defaultModel ?? ""}
              onChange={(event) => updateDefaultModel(event.currentTarget.value)}
            >
              <option value="">{t("workspaceDefaultModel")}</option>
              {modelContext.modelOptions.map((option) => (
                <option key={option.modelName} value={option.modelName}>{option.modelName}</option>
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
          <div className="settings-choice-group" role="group" aria-label={t("appearance")}>
            {themeOptions.map((option) => (
              <Button
                key={option.value}
                type="button"
                variant={appearance.preference === option.value ? "primary" : "secondary"}
                leadingIcon={<Icon name={option.icon} />}
                aria-pressed={appearance.preference === option.value}
                disabled={appearance.status === "saving"}
                onClick={() => void appearance.setPreference(option.value)}
              >
                {themeName(option.value, t)}
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

function themeName(preference: ThemePreference, t: ReturnType<typeof useLocale>["t"]): string {
  switch (preference) {
    case "system": return t("systemTheme");
    case "light": return t("lightTheme");
    case "dark": return t("darkTheme");
  }
}
