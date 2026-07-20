export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export interface AppearanceSnapshot {
  preference: ThemePreference;
  resolvedTheme: ResolvedTheme;
}

export const DEFAULT_APPEARANCE: AppearanceSnapshot = {
  preference: "system",
  resolvedTheme: "dark",
};
