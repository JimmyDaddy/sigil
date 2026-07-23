(() => {
  const candidate = window.__SIGIL_THEME_PREFERENCE__;
  const preferences = new Set([
    "system",
    "sigil_light",
    "sigil_dark",
    "solarized_light",
    "solarized_dark",
    "gruvbox_dark",
    "nord",
    "high_contrast_dark",
  ]);
  const preference = preferences.has(candidate) ? candidate : "system";
  const resolvedTheme = preference === "system"
    ? (window.matchMedia("(prefers-color-scheme: light)").matches ? "sigil_light" : "sigil_dark")
    : preference;
  const colorScheme = resolvedTheme === "sigil_light" || resolvedTheme === "solarized_light"
    ? "light"
    : "dark";
  const root = document.documentElement;
  root.dataset.themePreference = preference;
  root.dataset.theme = resolvedTheme;
  root.dataset.colorScheme = colorScheme;
  root.style.colorScheme = colorScheme;
})();
