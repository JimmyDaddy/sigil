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

  const compactNavigation = window.matchMedia("(max-width: 980px)");

  function applyNavigationMode() {
    document.querySelectorAll("details.nav-menu, details.doc-navigation").forEach((menu) => {
      if (compactNavigation.matches) {
        menu.removeAttribute("open");
      } else {
        menu.setAttribute("open", "");
      }
    });
  }

  function attachCompactNavigationDismissal() {
    document.querySelectorAll("details.nav-menu nav a").forEach((link) => {
      link.addEventListener("click", () => {
        if (compactNavigation.matches) {
          link.closest("details.nav-menu")?.removeAttribute("open");
        }
      });
    });
  }

  function attachTableOfContentsState() {
    const links = [...document.querySelectorAll(".doc-toc a[href^='#'], .doc-toc-mobile a[href^='#']")];
    if (links.length === 0) {
      return;
    }

    const linksById = new Map();
    links.forEach((link) => {
      const id = decodeURIComponent(link.hash.slice(1));
      const matches = linksById.get(id) || [];
      matches.push(link);
      linksById.set(id, matches);
    });

    const setActive = (id) => {
      links.forEach((link) => {
        const active = decodeURIComponent(link.hash.slice(1)) === id;
        link.classList.toggle("active", active);
        if (active) {
          link.setAttribute("aria-current", "location");
        } else {
          link.removeAttribute("aria-current");
        }
      });
    };

    const initialId = decodeURIComponent(window.location.hash.slice(1));
    if (initialId && linksById.has(initialId)) {
      setActive(initialId);
    }

    window.addEventListener("hashchange", () => {
      const id = decodeURIComponent(window.location.hash.slice(1));
      if (linksById.has(id)) {
        setActive(id);
      }
    });

    if (!("IntersectionObserver" in window)) {
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((entry) => entry.isIntersecting)
          .sort((left, right) => left.boundingClientRect.top - right.boundingClientRect.top);
        if (visible[0]) {
          setActive(visible[0].target.id);
        }
      },
      { rootMargin: "-18% 0px -72% 0px" }
    );
    linksById.forEach((_matchingLinks, id) => {
      const heading = document.getElementById(id);
      if (heading) {
        observer.observe(heading);
      }
    });
  }

  document.querySelectorAll("[data-theme-toggle]").forEach((button) => {
    button.addEventListener("click", toggleTheme);
  });

  media.addEventListener("change", () => {
    if (!storedTheme()) {
      applyTheme();
    }
  });

  compactNavigation.addEventListener("change", applyNavigationMode);

  window.addEventListener("storage", (event) => {
    if (event.key === storageKey) {
      applyTheme();
    }
  });

  applyTheme();
  applyNavigationMode();
  attachCompactNavigationDismissal();
  attachTableOfContentsState();
})();
