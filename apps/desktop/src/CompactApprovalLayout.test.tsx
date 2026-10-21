import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createRef } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { ApprovalDock } from "./ApprovalDock";
import { Composer } from "./Composer";
import { LocaleProvider } from "./i18n";
import { editorText } from "./test/editor";
import type { TimelineApproval } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

function featureStyles(shortViewport: boolean): string {
  const style = document.createElement("style");
  style.textContent = readFileSync(resolve(process.cwd(), "src/features/conversation/conversation.css"), "utf8");
  document.head.append(style);
  const features = Array.from(style.sheet!.cssRules).find((rule) =>
    "name" in rule && rule.name === "features"
  ) as CSSGroupingRule;
  const rules = Array.from(features.cssRules);
  const compact = rules.find((rule) => rule instanceof CSSMediaRule
    && rule.conditionText === "(max-height: 640px)") as CSSMediaRule;
  expect(compact).toBeTruthy();
  for (const rule of Array.from(compact.cssRules) as CSSStyleRule[]) {
    expect(rule.selectorText).toContain('.conversation-panel:has(.composer-document[data-review-active="true"])');
    if (rule.selectorText.endsWith(" .approval-dock")) {
      expect(rule.style.padding).toBe("var(--sg-sys-space-3)");
    }
  }
  // jsdom has no viewport layout and does not apply CSS layers/media queries.
  // Apply the selected real media branch explicitly to check its scope and declarations.
  const css = rules.flatMap((rule) => rule === compact
    ? shortViewport ? Array.from(compact.cssRules, (child) => child.cssText) : []
    : [rule.cssText]).join("\n");
  style.remove();
  return css;
}

it("reserves the short approval budget without changing idle or tall-window spacing", () => {
  const ref = createRef<HTMLElement>();
  const onDecision = vi.fn();
  const draft = "# 后续消息\n\n" + "保留完整 Markdown 草稿与尾空格。  \n".repeat(60);
  window.localStorage.setItem("compact-layout-draft", draft);
  const approval: TimelineApproval = {
    callId: "compact-call", toolName: "exec_command", approvalRequestId: "compact-request",
    toolCallHash: "a".repeat(64), policyVersion: "manual", expiresAtMs: Number.MAX_SAFE_INTEGER,
    snapshotRequired: false, operation: "execute_unknown_command", risk: "high",
    previewTitle: "Review the exact command", previewBody: "printf 'exact action'\n".repeat(100) + "EXACT TAIL",
  };
  const noop = () => undefined;
  const view = (reviewActive: boolean, shortViewport: boolean) => <LocaleProvider>
    <style>{featureStyles(shortViewport)}</style>
    <div className="conversation-layout"><section className="conversation-panel">
      <header className="conversation-header">Conversation</header>
      <div className="timeline"><p>Scrollable history</p></div>
      <ApprovalDock approval={approval} phase="pending" busy={false} composerRef={ref}
        onDecision={onDecision} onOpenSettings={noop} onOpenSupport={noop} />
      <Composer draftKey="compact-layout-draft" active={reviewActive} reviewActive={reviewActive}
        submissionBlocked={false} submitting={false} controlBusy={false} composerRef={ref}
        runContextBusy={false} permissionMode="manual" queueCount={0} queuePaused={false} queueBusy={false}
        onModelChange={noop} onPermissionModeChange={noop} onReasoningEffortChange={noop}
        onNewSession={async () => false} onOpenSessionPicker={noop} onOpenSettings={noop}
        onOpenSupport={noop} onOpenAgentWorkbench={noop} onOpenQueue={noop} onCompact={async () => false}
        onOpenIntentStack={noop} onNotice={noop} onSubmit={async () => false} onCancel={noop} />
    </section></div>
  </LocaleProvider>;
  const rendered = render(view(true, true));
  const panel = rendered.container.querySelector(".conversation-panel")!;
  const timeline = rendered.container.querySelector(".timeline")!;
  const dock = rendered.container.querySelector(".approval-dock")!;
  const composer = rendered.container.querySelector(".composer")!;
  const review = screen.getByRole("region", { name: "Requested action" });
  const editor = screen.getByRole("combobox", { name: "Message Sigil" });
  expect(getComputedStyle(panel).gap).toBe("var(--sg-sys-space-2)");
  expect(getComputedStyle(panel).paddingBlock).toBe("var(--sg-sys-space-2)");
  expect(getComputedStyle(timeline).paddingBlock).toBe("var(--sg-sys-space-1)");
  expect(getComputedStyle(timeline).minHeight).toBe("0px");
  expect(getComputedStyle(timeline).overflowY).toBe("auto");
  expect(getComputedStyle(dock).gap).toBe("var(--sg-sys-space-2)");
  expect(getComputedStyle(dock).minHeight).toBe("180px");
  expect(getComputedStyle(dock).gridTemplateRows).toBe("auto minmax(0, 1fr) auto");
  expect(getComputedStyle(review).overflowY).toBe("auto");
  expect(review.textContent).toContain("EXACT TAIL");
  expect(review.contains(screen.getByRole("button", { name: "Approve once" }))).toBe(false);
  expect(getComputedStyle(composer).flexShrink).toBe("0");
  expect(getComputedStyle(composer).paddingTop).toBe("0px");
  expect(editorText(editor)).toBe(draft);
  expect(screen.getByRole("button", { name: "Cancel work" })).toBeTruthy();

  for (const [reviewActive, shortViewport] of [[false, true], [true, false]]) {
    rendered.rerender(view(reviewActive, shortViewport));
    expect(getComputedStyle(panel).gap).toBe("14px");
    expect(getComputedStyle(composer).paddingTop).toBe("4px");
    expect(screen.getByRole("combobox", { name: "Message Sigil" })).toBe(editor);
    expect(editorText(editor)).toBe(draft);
  }
  expect(onDecision).not.toHaveBeenCalled();
});
