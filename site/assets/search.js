(() => {
  const indexCache = new Map();
  const maxResults = 8;
  const codeBlockLineThreshold = 18;
  const codeCopyFeedbackMs = 1300;
  const messages = {
    en: {
      empty: "No matching docs.",
      loading: "Loading search index...",
      error: "Search index could not be loaded.",
      copy: "Copy",
      copied: "Copied",
      failed: "Failed",
      expand: "Show more",
      collapse: "Show less"
    },
    "zh-CN": {
      empty: "没有匹配的文档。",
      loading: "正在加载搜索索引...",
      error: "无法加载搜索索引。",
      copy: "复制",
      copied: "已复制",
      failed: "失败",
      expand: "展开更多",
      collapse: "收起"
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

  async function copyTextToClipboard(text) {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return;
    }

    const fallback = document.createElement("textarea");
    fallback.value = text;
    fallback.style.position = "fixed";
    fallback.style.left = "-9999px";
    document.body.appendChild(fallback);
    fallback.select();
    fallback.setSelectionRange(0, 999_999);
    document.execCommand("copy");
    fallback.remove();
  }

  function blockLocale(block) {
    if (!block) {
      return "en";
    }

    const lang = String(block.lang || block.closest("html")?.getAttribute("lang") || "en").toLowerCase();
    if (lang.startsWith("zh")) {
      return "zh-CN";
    }
    return "en";
  }

  function createFeedbackButtonText(button, baseText, temporaryText) {
    if (button.dataset.timerId) {
      clearTimeout(Number(button.dataset.timerId));
      button.textContent = baseText;
    }

    button.textContent = temporaryText;
    button.dataset.timerId = String(window.setTimeout(() => {
      button.textContent = baseText;
      delete button.dataset.timerId;
    }, codeCopyFeedbackMs));
  }

  function enhanceCodeBlock(block, code, toolbar) {
    const language = blockLocale(block);
    const localeMessages = messages[language] || messages.en;
    const rawText = code.textContent || "";
    code.dataset.rawText = rawText;
    const lines = rawText.replace(/\r\n/g, "\n").replace(/\n$/, "").split("\n");
    const shouldNumberLines = !!block.closest(".doc-content");

    if (!shouldNumberLines) {
      return {
        copyLabel: localeMessages.copy,
        copiedLabel: localeMessages.copied,
        failedLabel: localeMessages.failed,
      };
    }

    const fragment = document.createDocumentFragment();
    lines.forEach((line, index) => {
      const lineElement = document.createElement("span");
      lineElement.className = "code-line";

      const numberElement = document.createElement("span");
      numberElement.className = "code-line-number";
      numberElement.textContent = String(index + 1);

      const contentElement = document.createElement("span");
      contentElement.className = "code-line-content";
      contentElement.textContent = line;

      lineElement.append(numberElement, contentElement);
      if (lines.length > codeBlockLineThreshold && index >= codeBlockLineThreshold) {
        lineElement.classList.add("code-line-hidden");
      }

      fragment.append(lineElement);
    });

    code.textContent = "";
    code.append(fragment);
    code.classList.add("code-with-line-numbers");
    block.classList.add("code-block");

    if (lines.length > codeBlockLineThreshold) {
      block.classList.add("code-block-collapsible", "code-block-collapsed");
      block.dataset.totalLines = String(lines.length);
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = "code-block-button code-collapse-button";
      toggle.textContent = localeMessages.expand;
      toggle.setAttribute("aria-label", `${localeMessages.expand}: ${localeMessages.copy}`);
      toggle.setAttribute("aria-expanded", "false");

      toggle.addEventListener("click", () => {
        const nowCollapsed = block.classList.toggle("code-block-collapsed");
        block.classList.toggle("code-block-expanded", !nowCollapsed);
        toggle.setAttribute("aria-expanded", String(!nowCollapsed));
        toggle.textContent = nowCollapsed ? localeMessages.expand : localeMessages.collapse;
      });

      if (toolbar) {
        toolbar.append(toggle);
      }
    }

    return {
      copyLabel: localeMessages.copy,
      copiedLabel: localeMessages.copied,
      failedLabel: localeMessages.failed,
    };
  }

  function addCodeCopyButtons(scope = document) {
    scope.querySelectorAll("pre").forEach((block) => {
      const code = block.querySelector("code");
      if (!code || block.querySelector(".copy-code-button")) {
        return;
      }

      const language = blockLocale(block);
      const localeMessages = messages[language] || messages.en;
      const toolbar = document.createElement("div");
      toolbar.className = "code-block-toolbar";
      const labels = enhanceCodeBlock(block, code, toolbar);

      const button = document.createElement("button");
      button.type = "button";
      button.className = "code-block-button copy-code-button";
      button.textContent = labels.copyLabel || localeMessages.copy;
      button.setAttribute("aria-label", `${localeMessages.copy} code block`);
      button.addEventListener("click", async () => {
        try {
          await copyTextToClipboard(code.dataset.rawText || "");
          createFeedbackButtonText(button, labels.copyLabel || localeMessages.copy, labels.copiedLabel || localeMessages.copied);
        } catch (_error) {
          createFeedbackButtonText(button, labels.copyLabel || localeMessages.copy, labels.failedLabel || localeMessages.failed);
        }
      });

      toolbar.append(button);

      block.append(toolbar);
    });
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
  addCodeCopyButtons(document);
})();
