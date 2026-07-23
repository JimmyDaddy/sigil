export const THEME_PREFERENCES = [
  "system",
  "sigil_light",
  "sigil_dark",
  "solarized_light",
  "solarized_dark",
  "gruvbox_dark",
  "nord",
  "high_contrast_dark",
] as const;

export const RESOLVED_THEMES = [
  "sigil_light",
  "sigil_dark",
  "solarized_light",
  "solarized_dark",
  "gruvbox_dark",
  "nord",
  "high_contrast_dark",
] as const;

export type ThemePreference = typeof THEME_PREFERENCES[number];
export type ResolvedTheme = typeof RESOLVED_THEMES[number];
export type ThemeColorScheme = "light" | "dark";

export interface AppearanceSnapshot {
  preference: ThemePreference;
  resolvedTheme: ResolvedTheme;
}

export const DEFAULT_APPEARANCE: AppearanceSnapshot = {
  preference: "system",
  resolvedTheme: "sigil_dark",
};
