import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { LocaleProvider } from "./i18n";
import { normalizeMarkdownFences, SafeMarkdown } from "./SafeMarkdown";

afterEach(cleanup);

describe("safe markdown fence recovery", () => {
  it("separates a closing fence attached to the last code line", () => {
    render(
      <LocaleProvider>
        <SafeMarkdown text={[
          "```",
          "┌──────────┐",
          "└──────────┘```",
          "",
          "**Bottleneck?**",
          "",
          "| Stage | Cost |",
          "|---|---|",
          "| Parse | High |",
        ].join("\n")} />
      </LocaleProvider>,
    );

    const codeBlock = screen.getByText("┌──────────┐", { exact: false }).closest(".code-block");
    expect(codeBlock).not.toBeNull();
    expect(within(codeBlock as HTMLElement).queryByText("Bottleneck?")).toBeNull();
    expect(screen.getByText("Bottleneck?").tagName).toBe("STRONG");
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("leaves valid fences and ordinary inline backticks unchanged", () => {
    const valid = "```rust\nfn main() {}\n```\n\nText with ``` inline markers.";
    expect(normalizeMarkdownFences(valid)).toBe(valid);
  });

  it("recovers an attached tilde fence without changing its marker length", () => {
    expect(normalizeMarkdownFences("~~~~text\nvalue~~~~\nAfter")).toBe(
      "~~~~text\nvalue\n~~~~\nAfter",
    );
  });
});
