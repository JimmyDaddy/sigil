(() => {
  const storageKey = "sigil.theme";
  const root = document.documentElement;
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const themeColor = document.querySelector('meta[name="theme-color"]');
  const labels = {
    en: {
      theme: "Theme",
      system: "Follow system",
      light: "Light",
      dark: "Dark",
    },
    "zh-CN": {
      theme: "主题",
      system: "跟随系统",
      light: "浅色",
      dark: "深色",
    },
  };
  const themeIcons = {
    system: "◐",
    light: "☀",
    dark: "☾",
  };

  function pageLocale() {
    return root.lang && root.lang.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
  }

  function selectedTheme() {
    try {
      const value = window.localStorage.getItem(storageKey);
      return value === "dark" || value === "light" ? value : "system";
    } catch (_error) {
      return "system";
    }
  }

  function setStoredTheme(value) {
    try {
      if (value === "system") {
        window.localStorage.removeItem(storageKey);
      } else {
        window.localStorage.setItem(storageKey, value);
      }
    } catch (_error) {
      // Theme persistence is best-effort; the current document still updates.
    }
  }

  function effectiveTheme() {
    const selected = selectedTheme();
    return selected === "system" ? (media.matches ? "dark" : "light") : selected;
  }

  function applyTheme() {
    const selected = selectedTheme();
    const resolvedTheme = effectiveTheme();
    if (selected === "dark" || selected === "light") {
      root.dataset.theme = selected;
    } else {
      delete root.dataset.theme;
    }

    if (themeColor) {
      themeColor.content = resolvedTheme === "dark" ? "#0d1117" : "#1ecfc5";
    }

    const localeLabels = labels[pageLocale()] || labels.en;
    document.querySelectorAll("[data-theme-menu]").forEach((menu) => {
      const summary = menu.querySelector("[data-theme-summary]");
      const icon = menu.querySelector("[data-theme-icon]");
      const summaryLabel = `${localeLabels.theme}: ${localeLabels[selected]}`;
      if (summary) {
        summary.dataset.themeState = selected;
        summary.setAttribute("aria-label", summaryLabel);
        summary.setAttribute("title", summaryLabel);
      }
      if (icon) {
        icon.textContent = themeIcons[selected];
      }
      menu.querySelectorAll("[data-theme-option]").forEach((button) => {
        button.setAttribute("aria-pressed", String(button.dataset.themeOption === selected));
      });
    });
  }

  function attachThemeMenus() {
    document.querySelectorAll("[data-theme-menu]").forEach((menu) => {
      menu.querySelectorAll("[data-theme-option]").forEach((button) => {
        button.addEventListener("click", () => {
          setStoredTheme(button.dataset.themeOption);
          applyTheme();
          menu.removeAttribute("open");
          menu.querySelector("[data-theme-summary]")?.focus();
        });
      });
      menu.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && menu.open) {
          menu.removeAttribute("open");
          menu.querySelector("[data-theme-summary]")?.focus();
        }
      });
    });

    document.addEventListener("click", (event) => {
      document.querySelectorAll("[data-theme-menu][open]").forEach((menu) => {
        if (!menu.contains(event.target)) {
          menu.removeAttribute("open");
        }
      });
    });
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

  const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const finePointer = window.matchMedia("(hover: hover) and (pointer: fine)");

  function attachHeroPointerEffects() {
    const hero = document.querySelector(".hero");
    const terminal = hero?.querySelector("[data-terminal-stage] .terminal-preview");
    if (!hero || !terminal || !finePointer.matches) {
      return;
    }

    let pointerFrame = 0;
    let pendingPointer = null;
    const reset = () => {
      pendingPointer = null;
      if (pointerFrame) {
        window.cancelAnimationFrame(pointerFrame);
        pointerFrame = 0;
      }
      hero.style.setProperty("--hero-pointer-x", "72%");
      hero.style.setProperty("--hero-pointer-y", "34%");
      terminal.style.setProperty("--terminal-tilt-x", "0deg");
      terminal.style.setProperty("--terminal-tilt-y", "0deg");
    };
    const renderPointer = () => {
      pointerFrame = 0;
      if (!pendingPointer || reducedMotion.matches) {
        return;
      }

      const bounds = hero.getBoundingClientRect();
      const x = Math.max(0, Math.min(1, (pendingPointer.x - bounds.left) / bounds.width));
      const y = Math.max(0, Math.min(1, (pendingPointer.y - bounds.top) / bounds.height));
      hero.style.setProperty("--hero-pointer-x", `${(x * 100).toFixed(2)}%`);
      hero.style.setProperty("--hero-pointer-y", `${(y * 100).toFixed(2)}%`);
      terminal.style.setProperty("--terminal-tilt-x", `${((0.5 - y) * 3.6).toFixed(2)}deg`);
      terminal.style.setProperty("--terminal-tilt-y", `${((x - 0.5) * 4.8).toFixed(2)}deg`);
    };

    hero.addEventListener(
      "pointermove",
      (event) => {
        if (reducedMotion.matches) {
          return;
        }
        pendingPointer = { x: event.clientX, y: event.clientY };
        if (!pointerFrame) {
          pointerFrame = window.requestAnimationFrame(renderPointer);
        }
      },
      { passive: true }
    );
    hero.addEventListener("pointerleave", reset);
    reducedMotion.addEventListener("change", (event) => {
      if (event.matches) {
        reset();
      }
    });
  }

  function attachSurfaceSpotlights() {
    const surfaces = document.querySelectorAll(
      ".install-card, .step, .feature-grid article, .doc-grid a, .resource-card, .resource-list a, .terminal-deck .visual-card, .split-list ul"
    );
    surfaces.forEach((surface) => {
      surface.classList.add("spotlight-surface");
      if (!finePointer.matches) {
        return;
      }

      surface.addEventListener(
        "pointermove",
        (event) => {
          if (reducedMotion.matches) {
            return;
          }
          const bounds = surface.getBoundingClientRect();
          surface.style.setProperty("--spotlight-x", `${event.clientX - bounds.left}px`);
          surface.style.setProperty("--spotlight-y", `${event.clientY - bounds.top}px`);
        },
        { passive: true }
      );
      surface.addEventListener("pointerleave", () => {
        surface.style.setProperty("--spotlight-x", "50%");
        surface.style.setProperty("--spotlight-y", "50%");
      });
    });
  }

  function attachRevealMotion() {
    const items = [
      ...document.querySelectorAll(
        ".section-heading, .install-card, .step, .feature-grid article, .doc-grid a, .split-list ul, .terminal-deck .visual-card, .resource-card, .resource-list a"
      ),
    ];
    if (items.length === 0 || reducedMotion.matches || !("IntersectionObserver" in window)) {
      return;
    }

    items.forEach((item, index) => {
      item.classList.add("reveal-item");
      item.style.setProperty("--reveal-delay", `${(index % 4) * 65}ms`);
    });
    root.classList.add("motion-enhanced");

    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (entry.isIntersecting) {
            entry.target.classList.add("is-visible");
            observer.unobserve(entry.target);
          }
        });
      },
      { rootMargin: "0px 0px -8% 0px", threshold: 0.12 }
    );
    items.forEach((item) => observer.observe(item));
  }

  function attachCapabilityMotionControls() {
    document.querySelectorAll("[data-capability-motion-toggle]").forEach((button) => {
      const rail = button.closest(".capability-rail");
      const icon = button.querySelector("[aria-hidden='true']");
      if (!rail || !icon) {
        return;
      }

      button.addEventListener("click", () => {
        const paused = rail.classList.toggle("is-paused");
        const label = paused ? button.dataset.resumeLabel : button.dataset.pauseLabel;
        button.setAttribute("aria-label", label);
        button.setAttribute("title", label);
        icon.textContent = paused ? "▶" : "Ⅱ";
      });
    });
  }

  media.addEventListener("change", () => {
    if (selectedTheme() === "system") {
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
  attachThemeMenus();
  applyNavigationMode();
  attachCompactNavigationDismissal();
  attachTableOfContentsState();
  attachHeroPointerEffects();
  attachSurfaceSpotlights();
  attachRevealMotion();
  attachCapabilityMotionControls();
})();
