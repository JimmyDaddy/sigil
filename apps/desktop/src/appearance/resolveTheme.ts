import {
  DEFAULT_APPEARANCE,
  type AppearanceSnapshot,
  type ResolvedTheme,
  type ThemePreference,
} from "./contract";

const preferences = new Set<ThemePreference>(["system", "light", "dark"]);
const resolvedThemes = new Set<ResolvedTheme>(["light", "dark"]);

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
  return window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function applyAppearance(snapshot: AppearanceSnapshot): void {
  const root = document.documentElement;
  root.dataset.themePreference = snapshot.preference;
  root.dataset.theme = snapshot.resolvedTheme;
  root.style.colorScheme = snapshot.resolvedTheme;
  const themeColor = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (themeColor !== null) {
    const canvas = getComputedStyle(root).getPropertyValue("--sg-sys-color-canvas").trim();
    if (canvas !== "") themeColor.content = canvas;
  }
}
