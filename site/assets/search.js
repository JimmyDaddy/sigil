(() => {
  const indexCache = new Map();
  const maxResults = 8;
  const ranking = window.SigilSearchRanking;
  if (!ranking) {
    return;
  }
  const messages = {
    en: {
      empty: "No matching docs.",
      loading: "Loading search index...",
      error: "Search index could not be loaded.",
    },
    "zh-CN": {
      empty: "没有匹配的文档。",
      loading: "正在加载搜索索引...",
      error: "无法加载搜索索引。",
    },
  };

  async function loadIndex(indexUrl) {
    if (!indexCache.has(indexUrl)) {
      indexCache.set(
        indexUrl,
        fetch(indexUrl, { credentials: "same-origin" }).then((response) => {
          if (!response.ok) {
            throw new Error(`Search index request failed: ${response.status}`);
          }
          return response.json();
        })
      );
    }
    return indexCache.get(indexUrl);
  }

  function resultHref(rootPrefix, item) {
    return `${rootPrefix}${item.url}`;
  }

  function renderStatus(container, locale, key) {
    container.replaceChildren();
    const status = document.createElement("p");
    status.className = "search-status";
    status.textContent = (messages[locale] || messages.en)[key];
    container.append(status);
  }

  function renderResults(container, rootPrefix, items) {
    container.replaceChildren();
    for (const item of items) {
      const link = document.createElement("a");
      link.className = "search-result";
      link.href = resultHref(rootPrefix, item);

      if (item.kind === "section") {
        const page = document.createElement("span");
        page.className = "search-result-page";
        page.textContent = item.title;
        link.append(page);
      }

      const title = document.createElement("span");
      title.className = "search-result-title";
      title.textContent = item.kind === "section" ? item.section : item.title;

      const description = document.createElement("span");
      description.className = "search-result-description";
      description.textContent = item.description;

      link.append(title, description);
      container.append(link);
    }
  }

  function resultLinks(container) {
    return [...container.querySelectorAll("a.search-result")];
  }

  function attachSearch(form) {
    const input = form.querySelector('input[type="search"]');
    const results = form.querySelector(".search-results");
    if (!input || !results) {
      return;
    }

    const locale = form.dataset.locale || "en";
    const indexUrl = form.dataset.index || "search.json";
    const rootPrefix = indexUrl.replace(/search\.json(?:\?.*)?$/, "");
    let loadedIndex = null;

    input.addEventListener("input", async () => {
      const query = input.value;
      const tokens = ranking.tokenize(query);
      if (tokens.length === 0) {
        results.replaceChildren();
        return;
      }

      try {
        if (!loadedIndex) {
          renderStatus(results, locale, "loading");
          loadedIndex = await loadIndex(indexUrl);
        }
        if (input.value !== query) {
          return;
        }

        const matches = ranking.rank(loadedIndex, query, locale, maxResults);

        if (matches.length === 0) {
          renderStatus(results, locale, "empty");
          return;
        }
        renderResults(results, rootPrefix, matches);
      } catch (_error) {
        renderStatus(results, locale, "error");
      }
    });

    input.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowDown") {
        return;
      }
      const links = resultLinks(results);
      if (links.length > 0) {
        event.preventDefault();
        links[0].focus();
      }
    });

    results.addEventListener("keydown", (event) => {
      const links = resultLinks(results);
      const index = links.indexOf(document.activeElement);
      if (index < 0) {
        return;
      }

      if (event.key === "Escape") {
        event.preventDefault();
        input.focus();
      } else if (event.key === "ArrowDown" && index < links.length - 1) {
        event.preventDefault();
        links[index + 1].focus();
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        (links[index - 1] || input).focus();
      }
    });

    form.addEventListener("submit", (event) => {
      event.preventDefault();
      resultLinks(results)[0]?.click();
    });
  }

  document.querySelectorAll(".site-search").forEach(attachSearch);
})();
