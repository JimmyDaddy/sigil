(() => {
  const storageKey = "sigil.theme";
  const root = document.documentElement;
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const themeColor = document.querySelector('meta[name="theme-color"]');
  const labels = {
    en: {
      dark: "Switch to dark theme",
      light: "Switch to light theme",
    },
    "zh-CN": {
      dark: "切换到深色主题",
      light: "切换到浅色主题",
    },
  };

  function pageLocale() {
    return root.lang && root.lang.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
  }

  function storedTheme() {
    try {
      const value = window.localStorage.getItem(storageKey);
      return value === "dark" || value === "light" ? value : null;
    } catch (_error) {
      return null;
    }
  }

  function setStoredTheme(value) {
    try {
      window.localStorage.setItem(storageKey, value);
    } catch (_error) {
      // Theme persistence is best-effort; the current document still updates.
    }
  }

  function effectiveTheme() {
    return storedTheme() || (media.matches ? "dark" : "light");
  }

  function applyTheme() {
    const selectedTheme = storedTheme();
    const resolvedTheme = effectiveTheme();
    if (selectedTheme) {
      root.dataset.theme = selectedTheme;
    } else {
      delete root.dataset.theme;
    }

    if (themeColor) {
      themeColor.content = resolvedTheme === "dark" ? "#0d1117" : "#1ecfc5";
    }

    const localeLabels = labels[pageLocale()] || labels.en;
    document.querySelectorAll("[data-theme-toggle]").forEach((button) => {
      const nextTheme = resolvedTheme === "dark" ? "light" : "dark";
      button.textContent = resolvedTheme === "dark" ? "☀" : "☾";
      button.dataset.themeState = resolvedTheme;
      button.setAttribute("aria-pressed", String(resolvedTheme === "dark"));
      button.setAttribute("aria-label", localeLabels[nextTheme]);
      button.setAttribute("title", localeLabels[nextTheme]);
    });
  }

  function toggleTheme() {
    setStoredTheme(effectiveTheme() === "dark" ? "light" : "dark");
    applyTheme();
  }

  document.querySelectorAll("[data-theme-toggle]").forEach((button) => {
    button.addEventListener("click", toggleTheme);
  });

  media.addEventListener("change", () => {
    if (!storedTheme()) {
      applyTheme();
    }
  });

  window.addEventListener("storage", (event) => {
    if (event.key === storageKey) {
      applyTheme();
    }
  });

  applyTheme();
})();
