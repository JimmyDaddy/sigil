import type { ThemePreference } from "./contract";
import { useAppearance } from "./ThemeProvider";
import { Icon, type IconName } from "../ui/icons";
import { useLocale } from "../i18n";
import { IconButton, Tooltip } from "../ui/primitives";

const nextPreference: Record<ThemePreference, ThemePreference> = {
  system: "light",
  light: "dark",
  dark: "system",
};

const preferenceIcon: Record<ThemePreference, IconName> = {
  system: "appearance-auto",
  light: "sun",
  dark: "moon",
};

export function AppearanceToggle() {
  const appearance = useAppearance();
  const { t } = useLocale();
  const next = nextPreference[appearance.preference];
  const failed = appearance.error !== undefined;
  const label = failed
    ? t("themeFailed")
    : t("switchTheme", {
      current: themeLabel(appearance.preference, t),
      next: themeLabel(next, t).toLocaleLowerCase(),
    });

  const changeAppearance = () => {
    if (failed) void appearance.retry();
    else void appearance.setPreference(next);
  };

  return (
    <span className={`appearance-toggle${failed ? " appearance-toggle-error" : ""}`}>
      <Tooltip label={label}>
        <IconButton
          aria-label={label}
          icon={<Icon name={preferenceIcon[appearance.preference]} />}
          type="button"
          disabled={appearance.status === "saving"}
          onClick={changeAppearance}
        />
      </Tooltip>
      {appearance.error === undefined ? null : (
        <span className="sr-only" role="alert">{appearance.error}</span>
      )}
    </span>
  );
}

function themeLabel(preference: ThemePreference, t: ReturnType<typeof useLocale>["t"]): string {
  switch (preference) {
    case "system": return t("systemTheme");
    case "light": return t("lightTheme");
    case "dark": return t("darkTheme");
  }
}
