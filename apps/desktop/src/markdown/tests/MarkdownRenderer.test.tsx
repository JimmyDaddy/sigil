import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "../../i18n";
import { SafeMarkdown } from "../../SafeMarkdown";

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    render: vi.fn(async (id: string) => ({
      svg: `<svg xmlns="http://www.w3.org/2000/svg" id="${id}"><text>diagram</text></svg>`,
    })),
  },
}));

afterEach(cleanup);

function renderMarkdown(text: string, phase: "streaming" | "complete" = "complete") {
  return render(
    <LocaleProvider>
      <div className="message-content">
        <SafeMarkdown text={text} phase={phase} contentId="renderer-test" />
      </div>
    </LocaleProvider>,
  );
}

describe("safe conversation Markdown renderer", () => {
  it("renders inline and display formulas with accessible MathML", async () => {
    const { container } = renderMarkdown("Inline $e^{i\\pi}+1=0$.\n\n$$\na^2+b^2=c^2\n$$");
    await waitFor(() => expect(container.querySelectorAll("math").length).toBe(2));
    expect(container.querySelector(".katex-display")).not.toBeNull();
  });

  it("keeps raw HTML and remote images outside the transcript DOM", () => {
    const { container } = renderMarkdown([
      '<img src="https://example.com/tracker.png" onerror="alert(1)">',
      "",
      "![remote](https://example.com/remote.png)",
    ].join("\n"));
    expect(container.querySelector("img")).toBeNull();
    expect(container.innerHTML).not.toContain("onerror");
  });

  it("keeps untrusted KaTeX commands from creating links, images, or trusted HTML", async () => {
    const { container } = renderMarkdown([
      "$$",
      String.raw`\href{javascript:alert(1)}{bad}`,
      String.raw`\includegraphics{https://example.com/tracker.png}`,
      String.raw`\htmlClass{danger}{text}`,
      "$$",
    ].join("\n"));
    await waitFor(() => expect(container.querySelector("math")).not.toBeNull());
    expect(container.querySelector("a")).toBeNull();
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector('[href^="javascript:"]')).toBeNull();
    expect(container.querySelector('[href^="https://example.com"]')).toBeNull();
    expect(container.innerHTML).not.toContain('class="danger"');
    expect(container.querySelector("annotation")?.textContent).toContain(String.raw`\href`);
  });

  it("renders a 64 KiB message without an unbounded document wrapper", () => {
    const text = Array.from(
      { length: 1_024 },
      (_, index) => `Paragraph ${index}: ${"bounded ".repeat(6)}`,
    ).join("\n\n");
    expect(new TextEncoder().encode(text).byteLength).toBeGreaterThanOrEqual(64 * 1_024);
    const { container } = renderMarkdown(text);
    expect(container.querySelectorAll("p")).toHaveLength(1_024);
    expect(container.querySelector(".message-content")?.scrollWidth ?? 0)
      .toBeLessThanOrEqual(container.querySelector(".message-content")?.clientWidth ?? 0);
  });

  it("keeps a stable prefix mounted while an append-only live tail grows", () => {
    const first = "Stable paragraph.\n\nLive";
    const view = renderMarkdown(first, "streaming");
    const stableNode = screen.getByText("Stable paragraph.");
    view.rerender(
      <LocaleProvider>
        <div className="message-content">
          <SafeMarkdown
            text={`${first} tail`}
            phase="streaming"
            contentId="renderer-test"
          />
        </div>
      </LocaleProvider>,
    );
    expect(screen.getByText("Stable paragraph.")).toBe(stableNode);
    expect(screen.getByText("Live tail")).toBeTruthy();
  });

  it("caps diagrams per message during streaming without accumulating across rerenders", async () => {
    const source = Array.from(
      { length: 17 },
      (_, index) => `\`\`\`mermaid\nflowchart TD\nA${index}-->B${index}\n\`\`\``,
    ).join("\n\n");
    const view = renderMarkdown(source, "streaming");
    await waitFor(() => expect(view.container.querySelectorAll('svg[role="img"]')).toHaveLength(16));
    expect(screen.getByText("A16-->B16", { exact: false })).toBeTruthy();

    view.rerender(
      <LocaleProvider>
        <div className="message-content">
          <SafeMarkdown text={source} phase="streaming" contentId="renderer-test" />
        </div>
      </LocaleProvider>,
    );
    await waitFor(() => expect(view.container.querySelectorAll('svg[role="img"]')).toHaveLength(16));
  });

  it("keeps diagram disclosure state across an unchanged parent rerender", async () => {
    const user = userEvent.setup();
    const source = "```mermaid\nflowchart TD\nA-->B\n```";
    const view = renderMarkdown(source);
    await screen.findByText("flowchart · ready");

    await user.click(screen.getByRole("button", { name: "Show source" }));
    await user.click(screen.getByRole("button", { name: "Expand diagram" }));
    expect(screen.getByRole("button", { name: "Hide source" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Close viewer" })).toBeTruthy();

    view.rerender(
      <LocaleProvider>
        <div className="message-content">
          <SafeMarkdown text={source} phase="complete" contentId="renderer-test" />
        </div>
      </LocaleProvider>,
    );

    expect(screen.getByRole("button", { name: "Hide source" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Close viewer" })).toBeTruthy();
  });
});

describe("complete Markdown preview work", () => {
  it("renders every paragraph without repeatedly encoding the entire completed source", () => {
    const source = Array.from({ length: 128 }, (_, index) => `Paragraph ${index}`).join("\n\n");
    const encode = vi.spyOn(TextEncoder.prototype, "encode");
    try {
      const { container } = renderMarkdown(source);
      expect(container.querySelectorAll("p")).toHaveLength(128);
      expect(container.textContent).toContain("Paragraph 127");
      const wholeSourceEncodings = encode.mock.calls.filter(([value]) => value === source).length;
      expect(wholeSourceEncodings).toBeLessThanOrEqual(2);
    } finally {
      encode.mockRestore();
    }
  });

  it("retains completed CRLF and exact code text after a streaming phase", () => {
    const source = "Before **中文**\r\n\r\n```text\r\n  exact code  \r\nend\r\n```\r\n\r\nAfter.";
    const view = renderMarkdown(source, "streaming");
    view.rerender(
      <LocaleProvider>
        <div className="message-content">
          <SafeMarkdown text={source} phase="complete" contentId="renderer-test" />
        </div>
      </LocaleProvider>,
    );
    expect(view.container.querySelector("strong")?.textContent).toBe("中文");
    const code = view.container.querySelector("pre code");
    expect(code?.textContent).toBe("  exact code  \r\nend\n");
    expect(view.container.querySelectorAll("pre")).toHaveLength(1);
    expect(screen.getByText("After.")).toBeTruthy();
  });

  it("keeps completed attached fence repair without changing code indentation", () => {
    const source = "Before.\n\n```text\n  exact code  \nend```\n\nAfter.";
    const view = renderMarkdown(source);
    expect(view.container.querySelector("pre code")?.textContent).toBe("  exact code  \nend\n");
    expect(view.container.querySelectorAll("pre")).toHaveLength(1);
    expect(screen.getByText("After.")).toBeTruthy();
  });
});

it.each(["message-content", "composer-preview"])("keeps mixed task text in normal flow with disabled, named checkboxes in %s", async (scope) => {
  const parsed = document.createElement("style");
  parsed.textContent = readFileSync(resolve(process.cwd(), "src/features/conversation/conversation.css"), "utf8");
  document.head.append(parsed);
  const features = Array.from(parsed.sheet!.cssRules).find((rule) => "name" in rule && rule.name === "features") as CSSGroupingRule;
  const style = document.createElement("style");
  style.textContent = Array.from(features.cssRules, (rule) => rule.cssText).join("\n");
  parsed.replaceWith(style);
  try {
    const source = [
      "- [ ] 确认场地；**重点**是读完 [资料](https://example.com/notes) 与 `code` 的尾部。",
      "  - [x] 子项 **完整** 与 `nested`。",
      "- [ ] 多段任务 **第一段**。",
      "",
      "  继续同一任务的第二段。",
      "",
      "  ```text",
      "  exact block  ",
      "  ```",
    ].join("\n");
    const { container } = render(<div className={scope}><SafeMarkdown text={source} /></div>);
    const tasks = container.querySelectorAll(".task-list-item");
    expect(tasks).toHaveLength(3);
    for (const task of tasks) expect(getComputedStyle(task).display).toBe("list-item");
    const firstParagraph = tasks[0].querySelector(":scope > p")!;
    expect(firstParagraph.textContent).toBe(" 确认场地；重点是读完 资料 与 code 的尾部。");
    expect(firstParagraph.querySelector("strong")?.textContent).toBe("重点");
    expect(firstParagraph.querySelector("a")?.getAttribute("href")).toBe("https://example.com/notes");
    expect(firstParagraph.querySelector("code")?.textContent).toBe("code");
    expect(tasks[0].querySelector("ul .task-list-item")).toBe(tasks[1]);
    const checkboxes = screen.getAllByRole("checkbox") as HTMLInputElement[];
    expect(checkboxes.map((checkbox) => [checkbox.disabled, checkbox.checked, checkbox.getAttribute("aria-label")])).toEqual([
      [true, false, "Incomplete task"], [true, true, "Completed task"], [true, false, "Incomplete task"],
    ]);
    await userEvent.click(checkboxes[0]);
    expect(checkboxes[0].checked).toBe(false);
    expect(within(tasks[2] as HTMLElement).getByText("继续同一任务的第二段。")).toBeTruthy();
    expect(tasks[2].querySelector("pre code")?.textContent).toBe("exact block  \n");
  } finally {
    style.remove();
  }
});
