import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { LocaleProvider } from "./i18n";
import { PlanCard } from "./PlanCard";
import type { ConversationPlanReview, PlanReviewDetail } from "./types";

afterEach(cleanup);

const review: ConversationPlanReview = {
  planId: "plan-exact", planHash: `sha256:${"a".repeat(64)}`, status: "draft_ready",
  summary: "An immutable plan", summaryTruncated: false, stepCount: 0, targetPathCount: 0,
  suggestedCheckCount: 0, source: "explicit_plan_command", allowedActions: ["run", "save", "revise", "reject"], stale: false,
};
const detail: PlanReviewDetail = {
  planId: review.planId, planHash: review.planHash!, source: review.source,
  summary: review.summary!, steps: [], targetPaths: [], suggestedChecks: [],
  notes: ["Captured from plain model output; structured fields are display-only and unavailable."],
  lineage: { source: {}, createdAtMs: 1 },
};
const props = {
  disabled: false, busy: false, failure: false, detailOpen: true, detailBusy: false, detailFailure: false,
  onOpenDetail: () => undefined, onCloseDetail: () => undefined,
};

it("shows the entire immutable Markdown plan instead of empty structured placeholders", () => {
  const onDecision = vi.fn();
  const markdown = [
    "", "# Exact first paragraph", "", "> 保留中文方案与原始风险。", "",
    "| File | Purpose |", "| --- | --- |", "| agenda.py | independent tool |", "",
    "```python", "    print('中文')  ", "```", "",
    ...Array.from({ length: 100 }, (_, i) => `${i + 1}. Read-only review item ${i}.`),
    "", "**Exact last paragraph**", "",
  ].join("\n");
  const frozen = Object.freeze({ ...detail, legacyMarkdown: markdown });
  render(<LocaleProvider><PlanCard {...props} review={review} detail={frozen} onDecision={onDecision} /></LocaleProvider>);
  const workbench = screen.getByTestId("plan-workbench");
  expect(within(workbench).getByRole("heading", { name: "Exact first paragraph" })).toBeTruthy();
  expect(within(workbench).getByText("Exact last paragraph").tagName).toBe("STRONG");
  expect(within(workbench).getByRole("table")).toBeTruthy();
  expect(workbench.querySelector("blockquote")?.textContent).toContain("保留中文方案与原始风险。");
  expect(workbench.querySelector("pre code")?.textContent).toBe("    print('中文')  \n");
  expect(workbench.textContent).toContain("Read-only review item 99.");
  expect(screen.queryByText("0 steps")).toBeNull();
  expect(screen.queryByText("0 target paths")).toBeNull();
  expect(screen.queryByText("0 suggested checks")).toBeNull();
  expect(within(workbench).queryByRole("heading", { name: "Steps" })).toBeNull();
  expect(within(workbench).queryByRole("heading", { name: "Notes" })).toBeNull();
  expect(workbench.textContent).not.toContain(detail.notes[0]);
  expect(workbench.querySelector('form, textarea, [contenteditable="true"]')).toBeNull();
  expect(frozen.legacyMarkdown).toBe(markdown);
  expect(onDecision).not.toHaveBeenCalled();
});

it("uses the existing safe Markdown boundary without image requests or executable plan controls", () => {
  const onDecision = vi.fn();
  render(<LocaleProvider><PlanCard {...props} review={review} detail={{ ...detail, legacyMarkdown: [
    "# Safe review", "", "[unsafe](javascript:alert(1))", "",
    "![remote image](https://images.example.test/plan.png)", "",
    '<img src="https://images.example.test/raw.png" onerror="alert(1)">',
    "<script>alert('untrusted')</script>", "", "```sh", "printf 'review only'", "```", "", "Safe tail",
  ].join("\n") }} onDecision={onDecision} /></LocaleProvider>);
  const workbench = screen.getByTestId("plan-workbench");
  expect(workbench.querySelector("script, img, iframe, object")).toBeNull();
  expect(workbench.querySelector('a[href^="javascript:"]')).toBeNull();
  expect(workbench.querySelector("pre")?.textContent).toContain("printf 'review only'");
  expect(within(workbench).queryByRole("button")).toBeNull();
  expect(within(workbench).getByText("Safe tail")).toBeTruthy();
  expect(onDecision).not.toHaveBeenCalled();
});

it("keeps structured steps, paths, checks, risks and allowed decisions intact", () => {
  const onDecision = vi.fn();
  const structuredReview = { ...review, stepCount: 1, targetPathCount: 1, suggestedCheckCount: 1, risk: "Workspace write requires review" };
  const structured: PlanReviewDetail = { ...detail, notes: ["Keep the original data unchanged"],
    steps: [{ stepId: "implement", title: "Implement the independent tool", detail: "Preserve the existing CLI",
      role: "executor", mode: "write", isolation: "sequential_workspace_write", dependsOn: ["inspect"],
      targetPaths: ["agenda.py"], suggestedChecks: [], risk: "Review all writes", notes: ["Use only the agreed scope"] }],
    targetPaths: ["agenda.py"], suggestedChecks: [{ checkSpecId: "unit", command: "python3", args: ["-m", "unittest", "test_agenda"], effect: "read_only" }],
  };
  const rendered = render(<LocaleProvider><PlanCard {...props} review={structuredReview} detail={structured} onDecision={onDecision} /></LocaleProvider>);
  const workbench = screen.getByTestId("plan-workbench");
  expect(within(workbench).getByRole("heading", { name: "Steps" })).toBeTruthy();
  expect(within(workbench).getByText("Implement the independent tool")).toBeTruthy();
  expect(within(workbench).getByText("Preserve the existing CLI")).toBeTruthy();
  expect(within(workbench).getByText("Review all writes")).toBeTruthy();
  expect(within(workbench).getByText("python3 -m unittest test_agenda")).toBeTruthy();
  expect(within(workbench).getByText("Implement the independent tool").closest("li")?.textContent).toContain("agenda.py");
  expect(within(workbench).getByRole("heading", { name: "Scope" }).closest("section")?.querySelector("code")?.textContent).toBe("agenda.py");
  expect(within(workbench).getByText("Keep the original data unchanged")).toBeTruthy();
  expect(screen.getByText("Workspace write requires review")).toBeTruthy();
  expect(screen.getByText("1 steps")).toBeTruthy();
  expect(screen.getByText("1 target paths")).toBeTruthy();
  expect(screen.getByText("1 suggested checks")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Run plan" }));
  expect(onDecision).toHaveBeenCalledExactlyOnceWith("run");
  rendered.rerender(<LocaleProvider><PlanCard {...props} disabled busy failure detailFailure review={{ ...structuredReview, allowedActions: ["save"] }} detail={structured} onDecision={onDecision} /></LocaleProvider>);
  expect(screen.queryByRole("button", { name: "Run plan" })).toBeNull();
  expect(screen.getAllByRole("alert")).toHaveLength(2);
  expect((screen.getByRole("button", { name: "Applying…" }) as HTMLButtonElement).disabled).toBe(true);
});

it("omits zero-count structure badges before the exact detail is available", () => {
  render(<LocaleProvider><PlanCard {...props} detailOpen={false} review={review} onDecision={vi.fn()} /></LocaleProvider>);
  expect(screen.queryByText("0 steps")).toBeNull();
  expect(screen.queryByText("0 target paths")).toBeNull();
  expect(screen.queryByText("0 suggested checks")).toBeNull();
  expect(screen.getByText("Explicit /plan")).toBeTruthy();
  expect(screen.queryByTestId("plan-workbench")).toBeNull();
});

it.each(["planId", "planHash"] as const)("does not present a detail for a different %s as the current immutable plan", (field) => {
  const onDecision = vi.fn();
  render(<LocaleProvider><PlanCard {...props} review={review} detail={{ ...detail, [field]: "other-identity", legacyMarkdown: "# Obsolete plan body" }} onDecision={onDecision} /></LocaleProvider>);
  expect(screen.queryByTestId("plan-workbench")).toBeNull();
  expect(screen.queryByText("Obsolete plan body")).toBeNull();
  expect(onDecision).not.toHaveBeenCalled();
});

function planStyles(width: number): string {
  const style = document.createElement("style");
  style.textContent = readFileSync(resolve(process.cwd(), "src/features/conversation/conversation.css"), "utf8");
  document.head.append(style);
  const features = Array.from(style.sheet!.cssRules).find((rule) =>
    "name" in rule && rule.name === "features"
  ) as CSSGroupingRule;
  // jsdom has no viewport layout and does not apply layers/media. Select the real narrow
  // branch explicitly to verify its cascade and DOM scroll ownership, never pixel geometry.
  const css = Array.from(features.cssRules).flatMap((rule) => {
    if (rule instanceof CSSMediaRule) {
      return rule.conditionText === "(max-width: 620px)" && width <= 620
        ? Array.from(rule.cssRules, (child) => child.cssText) : [];
    }
    return [rule.cssText];
  }).join("\n");
  style.remove();
  return css;
}

it.each([[320, 560], [840, 560], [1440, 900]])(
  "keeps complete review content and fixed decisions in separate regions at the %ix%i CSS contract",
  (width, height) => {
    const onDecision = vi.fn();
    const longReview = { ...review, summary: "计划标题与范围。".repeat(50), risk: "Review the full immutable scope", stale: true };
    const markdown = "# Exact beginning\n\n" + "- 原始计划完整内容。\n".repeat(300) + "\nExact final risk";
    const rendered = render(<LocaleProvider><style>{planStyles(width)}</style>
      <div style={{ width, height }}><PlanCard {...props} review={longReview}
        detail={{ ...detail, legacyMarkdown: markdown }} onDecision={onDecision} /></div>
    </LocaleProvider>);
    const content = screen.getByRole("region", { name: "Plan review" });
    const card = rendered.container.querySelector(".plan-card")!;
    const footer = rendered.container.querySelector(".plan-card-footer")!;
    expect(getComputedStyle(card).display).toBe("flex");
    expect(getComputedStyle(card).flexDirection).toBe("column");
    expect(getComputedStyle(card).overflow).toBe("hidden");
    expect(getComputedStyle(content).minHeight).toBe("0px");
    expect(getComputedStyle(content).gridAutoRows).toBe("max-content");
    expect(getComputedStyle(content).overflowY).toBe("auto");
    expect(getComputedStyle(footer).flexShrink).toBe("0");
    expect(getComputedStyle(screen.getByRole("heading", { level: 3 })).overflowWrap).toBe("anywhere");
    const toolbarDirection = getComputedStyle(footer.querySelector(".plan-card-toolbar")!).flexDirection;
    if (width <= 620) expect(toolbarDirection).toBe("column");
    else expect(toolbarDirection).not.toBe("column");
    expect(getComputedStyle(footer.querySelector(".plan-card-actions")!).flexWrap).toBe("wrap");
    expect(within(content).getByRole("heading", { name: "Exact beginning" })).toBeTruthy();
    expect(within(content).getByText("Exact final risk")).toBeTruthy();
    expect(within(content).getByText(longReview.risk)).toBeTruthy();
    expect(within(content).getByRole("status")).toBeTruthy();
    expect(within(footer as HTMLElement).getByRole("button", { name: "Close complete plan" }).getAttribute("aria-controls")).toBe(content.id);
    expect(content.contains(footer)).toBe(false);
    for (const [name, action] of [["Run plan", "run"], ["Save", "save"], ["Revise", "revise"], ["Reject", "reject"]]) {
      fireEvent.click(within(footer as HTMLElement).getByRole("button", { name }));
      expect(onDecision).toHaveBeenLastCalledWith(action);
    }
    expect(onDecision).toHaveBeenCalledTimes(4);
    expect(markdown.endsWith("Exact final risk")).toBe(true);
  },
);

it("focuses a newly opened exact review for browser scrolling without stealing same-plan action focus", async () => {
  const user = userEvent.setup();
  const onDecision = vi.fn();
  const onOpenDetail = vi.fn();
  const longDetail = { ...detail, legacyMarkdown: "# Start\n\n" + "Read the complete plan.\n\n".repeat(200) + "Exact tail" };
  const rendered = render(<LocaleProvider><PlanCard {...props} detailOpen={false} review={review}
    onOpenDetail={onOpenDetail} onDecision={onDecision} /></LocaleProvider>);
  await user.click(screen.getByRole("button", { name: "Review complete plan" }));
  expect(onOpenDetail).toHaveBeenCalledOnce();
  rendered.rerender(<LocaleProvider><PlanCard {...props} review={review} detail={longDetail} onDecision={onDecision} /></LocaleProvider>);
  const content = screen.getByRole("region", { name: "Plan review" });
  expect(document.activeElement).toBe(content);
  for (const key of ["PageDown", "End", "Home", "ArrowDown"]) {
    const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
    expect(content.dispatchEvent(event)).toBe(true);
    expect(event.defaultPrevented).toBe(false);
  }
  expect(onDecision).not.toHaveBeenCalled();
  const save = screen.getByRole("button", { name: "Save" });
  save.focus();
  rendered.rerender(<LocaleProvider><PlanCard {...props} review={{ ...review, stale: true }}
    detail={{ ...longDetail }} onDecision={onDecision} /></LocaleProvider>);
  expect(document.activeElement).toBe(save);
  expect(within(content).getByText("Exact tail")).toBeTruthy();
  rendered.rerender(<LocaleProvider><PlanCard {...props} review={review}
    detail={{ ...longDetail, planHash: "wrong-frozen-plan" }} onDecision={onDecision} /></LocaleProvider>);
  expect(document.activeElement).toBe(save);
  expect(screen.queryByText("Exact tail")).toBeNull();
});

it("preserves a complete unconfirmed candidate in the same scroll region with only its allowed recovery action", () => {
  const content = "Exact candidate beginning\n" + "Original unconfirmed plan.\n".repeat(300) + "Exact candidate tail";
  const pending: ConversationPlanReview = { ...review, planHash: undefined, status: "paused",
    candidate: { contentHash: `sha256:${"b".repeat(64)}`, content, completeness: "complete" },
    allowedActions: ["retry_review"],
  };
  const onDecision = vi.fn();
  const rendered = render(<LocaleProvider><style>{planStyles(320)}</style>
    <PlanCard {...props} detailOpen={false} review={pending} onDecision={onDecision} />
  </LocaleProvider>);
  const region = screen.getByRole("region", { name: "Plan review" });
  const pre = region.querySelector("pre")!;
  expect(pre.textContent).toBe(content);
  expect(getComputedStyle(pre).overflow).toBe("visible");
  expect(getComputedStyle(pre).maxHeight).toBe("none");
  expect(screen.queryByRole("button", { name: "Run plan" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Review complete plan" })).toBeNull();
  const retry = rendered.container.querySelector<HTMLButtonElement>('[data-plan-action="retry_review"]')!;
  expect(region.contains(retry)).toBe(false);
  fireEvent.click(retry);
  expect(onDecision).toHaveBeenCalledExactlyOnceWith("retry_review");
});
