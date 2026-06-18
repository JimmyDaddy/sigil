(() => {
  const indexCache = new Map();
  const maxResults = 8;
  const messages = {
    en: {
      empty: "No matching docs.",
      loading: "Loading search index...",
      error: "Search index could not be loaded."
    },
    "zh-CN": {
      empty: "没有匹配的文档。",
      loading: "正在加载搜索索引...",
      error: "无法加载搜索索引。"
    }
  };

  function normalize(value) {
    return String(value || "").toLowerCase();
  }

  function tokenize(query) {
    return normalize(query)
      .split(/[\s,.;:!?()[\]{}"'`/\\]+/)
      .map((token) => token.trim())
      .filter(Boolean);
  }

  function scoreItem(item, tokens) {
    const title = normalize(item.title);
    const description = normalize(item.description);
    const text = normalize(item.text);
    let score = 0;

    for (const token of tokens) {
      let matched = false;
      if (title.includes(token)) {
        score += title.startsWith(token) ? 18 : 12;
        matched = true;
      }
      if (description.includes(token)) {
        score += 7;
        matched = true;
      }
      if (text.includes(token)) {
        score += 2;
        matched = true;
      }
      if (!matched) {
        return 0;
      }
    }

    return score;
  }

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

      const title = document.createElement("span");
      title.className = "search-result-title";
      title.textContent = item.title;

      const description = document.createElement("span");
      description.className = "search-result-description";
      description.textContent = item.description;

      link.append(title, description);
      container.append(link);
    }
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
      const tokens = tokenize(input.value);
      if (tokens.length === 0) {
        results.replaceChildren();
        return;
      }

      try {
        if (!loadedIndex) {
          renderStatus(results, locale, "loading");
          loadedIndex = await loadIndex(indexUrl);
        }

        const matches = loadedIndex
          .filter((item) => item.locale === locale)
          .map((item) => ({ item, score: scoreItem(item, tokens) }))
          .filter((entry) => entry.score > 0)
          .sort((left, right) => right.score - left.score || left.item.title.localeCompare(right.item.title))
          .slice(0, maxResults)
          .map((entry) => entry.item);

        if (matches.length === 0) {
          renderStatus(results, locale, "empty");
          return;
        }
        renderResults(results, rootPrefix, matches);
      } catch (_error) {
        renderStatus(results, locale, "error");
      }
    });

    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const firstResult = results.querySelector("a");
      if (firstResult) {
        firstResult.click();
      }
    });
  }

  document.querySelectorAll(".site-search").forEach(attachSearch);
})();
