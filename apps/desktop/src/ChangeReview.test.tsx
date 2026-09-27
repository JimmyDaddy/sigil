import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { ChangeReview } from "./ChangeReview";
import { emptyChangeReviewDraft, type CheckpointReview, type ReviewAnnotation } from "./features/conversation/reviewTypes";
import { LocaleProvider } from "./i18n";

afterEach(() => { cleanup(); window.localStorage.clear(); });
const review: CheckpointReview = {
  checkpointId: "cp", checkpointDigest: "checkpoint-digest", truncated: false,
  diffs: [{ path: "src/a.ts", sourceCallId: "call-a", diffDigest: "recorded-diff", fileState: "changed", truncated: false,
    lines: [{ text: "@@ -1,2 +1,2 @@" }, { text: " keep", oldLine: 1, newLine: 1 }, { text: "-old", oldLine: 2 }, { text: "+new", newLine: 2 }] }],
};
function Harness({ send }: { send: (annotations: ReviewAnnotation[]) => Promise<boolean> }) {
  const [draft, setDraft] = useState(emptyChangeReviewDraft);
  return <LocaleProvider><ChangeReview review={review} draft={draft} onDraftChange={setDraft} onRefresh={() => undefined} onSend={send} /></LocaleProvider>;
}

it("sends an outdated original range and preserves the batch through deferred failure and retry", async () => {
  let resolve: ((accepted: boolean) => void) | undefined;
  const send = vi.fn(() => new Promise<boolean>((done) => { resolve = done; }));
  const user = userEvent.setup();
  render(<Harness send={send} />);
  expect(screen.getByText("Outdated version")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Select src/a.ts new line 1" }));
  fireEvent.click(screen.getByRole("button", { name: "Select src/a.ts new line 2" }), { shiftKey: true });
  await user.type(screen.getByRole("textbox"), "Please clarify the new behavior");
  await user.click(screen.getByRole("button", { name: "Add comment" }));
  await user.click(screen.getByRole("button", { name: "Send comments" }));
  expect(screen.getByRole("button", { name: "Sending comments…" })).toBeTruthy();
  expect(screen.getByText("Please clarify the new behavior")).toBeTruthy();
  expect(send).toHaveBeenCalledWith([{ checkpointId: "cp", checkpointDigest: "checkpoint-digest", sourceCallId: "call-a", diffDigest: "recorded-diff", path: "src/a.ts", side: "new", startLine: 1, endLine: 2, comment: "Please clarify the new behavior" }]);
  await act(async () => resolve?.(false));
  expect(screen.getByRole("alert").textContent).toContain("preserved");
  expect(screen.getByText("Please clarify the new behavior")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Send comments" }));
  await act(async () => resolve?.(true));
  await waitFor(() => expect(screen.queryByText("Please clarify the new behavior")).toBeNull());
});

it("keeps comments added while an earlier batch is being submitted", async () => {
  let resolve: ((accepted: boolean) => void) | undefined;
  const send = vi.fn(() => new Promise<boolean>((done) => { resolve = done; }));
  const user = userEvent.setup();
  render(<Harness send={send} />);
  await user.click(screen.getByRole("button", { name: "Select src/a.ts old line 2" }));
  await user.type(screen.getByRole("textbox"), "First comment");
  await user.click(screen.getByRole("button", { name: "Add comment" }));
  await user.click(screen.getByRole("button", { name: "Send comments" }));
  await user.type(screen.getByRole("textbox"), "Second comment while pending");
  await user.click(screen.getByRole("button", { name: "Add comment" }));
  await act(async () => resolve?.(true));
  expect(screen.queryByText("First comment")).toBeNull();
  expect(screen.getByText("Second comment while pending")).toBeTruthy();
});
