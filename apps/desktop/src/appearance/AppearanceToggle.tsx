import type { ThemePreference } from "./contract";
import { useAppearance } from "./ThemeProvider";
import { Icon, type IconName } from "../ui/icons";
import { IconButton, Tooltip } from "../ui/primitives";

const nextPreference: Record<ThemePreference, ThemePreference> = {
  system: "light",
  light: "dark",
  dark: "system",
};

const preferenceIcon: Record<ThemePreference, IconName> = {
  system: "system",
  light: "sun",
  dark: "moon",
};

const preferenceLabel: Record<ThemePreference, string> = {
  system: "System theme",
  light: "Light theme",
  dark: "Dark theme",
};

export function AppearanceToggle() {
  const appearance = useAppearance();
  const next = nextPreference[appearance.preference];
  const failed = appearance.error !== undefined;
  const label = failed
    ? "Theme change failed. Retry"
    : `${preferenceLabel[appearance.preference]}. Switch to ${preferenceLabel[next].toLowerCase()}`;

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
