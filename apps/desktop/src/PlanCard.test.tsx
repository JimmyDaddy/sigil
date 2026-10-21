import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
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
