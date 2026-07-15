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
  attachHeroPointerEffects();
  attachSurfaceSpotlights();
  attachRevealMotion();
  attachCapabilityMotionControls();
})();
