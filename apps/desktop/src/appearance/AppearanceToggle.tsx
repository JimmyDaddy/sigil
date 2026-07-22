import { useEffect, useRef } from "react";

import type { ThemePreference } from "./contract";
import { useAppearance } from "./ThemeProvider";
import { Icon, type IconName } from "../ui/icons";
import { useLocale } from "../i18n";
import { useNotifications } from "../ui/feedback";
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
  const { dismiss, notify } = useNotifications();
  const notifiedError = useRef<string | undefined>(undefined);
  const errorNotificationId = useRef<number | undefined>(undefined);
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

  useEffect(() => {
    if (appearance.error === undefined) {
      if (errorNotificationId.current !== undefined) dismiss(errorNotificationId.current);
      errorNotificationId.current = undefined;
      notifiedError.current = undefined;
      return;
    }
    if (notifiedError.current === appearance.error) return;
    notifiedError.current = appearance.error;
    errorNotificationId.current = notify({ message: appearance.error, tone: "error" });
  }, [appearance.error, dismiss, notify]);

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
