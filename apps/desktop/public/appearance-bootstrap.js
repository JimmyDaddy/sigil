(() => {
  const candidate = window.__SIGIL_THEME_PREFERENCE__;
  const preference = candidate === "light" || candidate === "dark" ? candidate : "system";
  const resolvedTheme = preference === "system"
    ? (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")
    : preference;
  const root = document.documentElement;
  root.dataset.themePreference = preference;
  root.dataset.theme = resolvedTheme;
  root.style.colorScheme = resolvedTheme;
})();
