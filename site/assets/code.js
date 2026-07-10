(() => {
  const lineThreshold = 18;
  const feedbackDurationMs = 1300;
  const messages = {
    en: {
      copy: "Copy",
      copyLabel: "Copy code block",
      copied: "Code copied",
      failed: "Copy failed",
      collapse: "Show fewer lines",
      expand: (hidden) => `Show ${hidden} more ${hidden === 1 ? "line" : "lines"}`,
      toolbar: "Code actions",
    },
    "zh-CN": {
      copy: "复制",
      copyLabel: "复制代码块",
      copied: "代码已复制",
      failed: "复制失败",
      collapse: "收起代码",
      expand: (hidden) => `展开其余 ${hidden} 行`,
      toolbar: "代码操作",
    },
  };

  function localeFor(element) {
    const language = String(element.closest("html")?.lang || "en").toLowerCase();
    return language.startsWith("zh") ? "zh-CN" : "en";
  }

  function normalizedCodeText(value) {
    return String(value || "")
      .replace(/\r\n/g, "\n")
      .replace(/^\n/, "")
      .replace(/\n$/, "");
  }

  async function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return;
    }

    const fallback = document.createElement("textarea");
    fallback.value = text;
    fallback.style.position = "fixed";
    fallback.style.left = "-9999px";
    document.body.append(fallback);
    fallback.select();
    fallback.setSelectionRange(0, 999_999);
    document.execCommand("copy");
    fallback.remove();
  }

  function ensureFrame(block) {
    if (block.parentElement?.classList.contains("code-block-frame")) {
      return block.parentElement;
    }

    const frame = document.createElement("div");
    frame.className = "code-block-frame";
    block.before(frame);
    frame.append(block);
    return frame;
  }

  function renderNumberedLines(block, code, rawText, frame, localeMessages) {
    if (!block.closest(".doc-content")) {
      return;
    }

    const lines = rawText.split("\n");
    const fragment = document.createDocumentFragment();
    lines.forEach((line, index) => {
      const row = document.createElement("span");
      row.className = "code-line";

      const number = document.createElement("span");
      number.className = "code-line-number";
      number.textContent = String(index + 1);
      number.setAttribute("aria-hidden", "true");

      const content = document.createElement("span");
      content.className = "code-line-content";
      content.textContent = line;

      row.append(number, content);
      if (lines.length > lineThreshold && index >= lineThreshold) {
        row.classList.add("code-line-hidden");
      }
      fragment.append(row);
    });

    code.replaceChildren(fragment);
    code.classList.add("code-with-line-numbers");

    if (lines.length <= lineThreshold) {
      return;
    }

    const hiddenLineCount = lines.length - lineThreshold;
    frame.classList.add("code-block-collapsible", "code-block-collapsed");
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "code-block-button code-collapse-button";
    toggle.setAttribute("aria-controls", code.id);
    toggle.setAttribute("aria-expanded", "false");

    const updateToggle = (collapsed) => {
      const label = collapsed ? localeMessages.expand(hiddenLineCount) : localeMessages.collapse;
      toggle.textContent = label;
      toggle.setAttribute("aria-label", label);
      toggle.setAttribute("aria-expanded", String(!collapsed));
    };
    updateToggle(true);

    toggle.addEventListener("click", () => {
      const collapsed = frame.classList.toggle("code-block-collapsed");
      frame.classList.toggle("code-block-expanded", !collapsed);
      updateToggle(collapsed);
    });
    frame.querySelector(".code-block-toolbar")?.prepend(toggle);
  }

  function addCodeActions(scope = document) {
    let nextId = 0;
    scope.querySelectorAll("pre").forEach((block) => {
      if (block.closest(".terminal-preview") || block.parentElement?.classList.contains("code-block-frame")) {
        return;
      }

      const code = block.querySelector("code");
      if (!code) {
        return;
      }

      const locale = localeFor(block);
      const localeMessages = messages[locale] || messages.en;
      const rawText = normalizedCodeText(code.textContent);
      code.dataset.rawText = rawText;
      code.id ||= `code-block-${++nextId}`;

      const frame = ensureFrame(block);
      const toolbar = document.createElement("div");
      toolbar.className = "code-block-toolbar";
      toolbar.setAttribute("role", "toolbar");
      toolbar.setAttribute("aria-label", localeMessages.toolbar);

      const copyButton = document.createElement("button");
      copyButton.type = "button";
      copyButton.className = "code-block-button copy-code-button";
      copyButton.textContent = localeMessages.copy;
      copyButton.setAttribute("aria-label", localeMessages.copyLabel);
      copyButton.setAttribute("aria-controls", code.id);

      const feedback = document.createElement("span");
      feedback.className = "visually-hidden code-feedback";
      feedback.setAttribute("role", "status");
      feedback.setAttribute("aria-live", "polite");

      copyButton.addEventListener("click", async () => {
        try {
          await copyText(rawText);
          copyButton.textContent = localeMessages.copied;
          feedback.textContent = localeMessages.copied;
        } catch (_error) {
          copyButton.textContent = localeMessages.failed;
          feedback.textContent = localeMessages.failed;
        }

        window.clearTimeout(Number(copyButton.dataset.timerId || 0));
        copyButton.dataset.timerId = String(window.setTimeout(() => {
          copyButton.textContent = localeMessages.copy;
          delete copyButton.dataset.timerId;
        }, feedbackDurationMs));
      });

      toolbar.append(copyButton, feedback);
      frame.append(toolbar);
      block.classList.add("code-block");
      renderNumberedLines(block, code, rawText, frame, localeMessages);
    });
  }

  addCodeActions();
})();
