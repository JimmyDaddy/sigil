import {
  DEFAULT_APPEARANCE,
  RESOLVED_THEMES,
  THEME_PREFERENCES,
  type AppearanceSnapshot,
  type ResolvedTheme,
  type ThemeColorScheme,
  type ThemePreference,
} from "./contract";

const preferences = new Set<ThemePreference>(THEME_PREFERENCES);
const resolvedThemes = new Set<ResolvedTheme>(RESOLVED_THEMES);

export function appearanceFromDocument(): AppearanceSnapshot {
  const preference = document.documentElement.dataset.themePreference;
  const resolvedTheme = document.documentElement.dataset.theme;
  return {
    preference: preferences.has(preference as ThemePreference)
      ? preference as ThemePreference
      : DEFAULT_APPEARANCE.preference,
    resolvedTheme: resolvedThemes.has(resolvedTheme as ResolvedTheme)
      ? resolvedTheme as ResolvedTheme
      : resolveSystemTheme(),
  };
}

export function resolveSystemTheme(): ResolvedTheme {
  return window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "sigil_light"
    : "sigil_dark";
}

export function themeColorScheme(theme: ResolvedTheme): ThemeColorScheme {
  return theme === "sigil_light" || theme === "solarized_light" ? "light" : "dark";
}

export function applyAppearance(snapshot: AppearanceSnapshot): void {
  const root = document.documentElement;
  const colorScheme = themeColorScheme(snapshot.resolvedTheme);
  root.dataset.themePreference = snapshot.preference;
  root.dataset.theme = snapshot.resolvedTheme;
  root.dataset.colorScheme = colorScheme;
  root.style.colorScheme = colorScheme;
  const themeColor = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (themeColor !== null) {
    const canvas = getComputedStyle(root).getPropertyValue("--sg-sys-color-canvas").trim();
    if (canvas !== "") themeColor.content = canvas;
  }
}
