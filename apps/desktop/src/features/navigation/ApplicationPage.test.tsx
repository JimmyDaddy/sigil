import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { ApplicationPage } from "./ApplicationPage";

afterEach(cleanup);

it("keeps one navigated page title and a working return action without an unused introduction", async () => {
  const onBack = vi.fn();
  const { container } = render(<ApplicationPage eyebrow="Repeated context" title="Library" navigation={{ label: "Back to conversation", onBack }} aside={<span>2 loaded</span>}><p>First useful content</p></ApplicationPage>);
  expect(screen.getAllByRole("heading", { name: "Library" })).toHaveLength(1);
  expect(screen.queryByText("Repeated context")).toBeNull();
  expect(container.querySelector(".application-page-intro")).toBeNull();
  expect(container.querySelector(".application-page-content")?.firstElementChild?.textContent).toBe("First useful content");
  expect(screen.getByText("2 loaded")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Back to conversation" }));
  expect(onBack).toHaveBeenCalledOnce();
});

it("preserves an explicit useful description without repeating the eyebrow beside a toolbar", () => {
  render(<ApplicationPage eyebrow="Repeated context" title="Page" detail="Necessary description" navigation={{ label: "Back", onBack: vi.fn() }}><p>Content</p></ApplicationPage>);
  expect(screen.getAllByText("Necessary description")).toHaveLength(1);
  expect(screen.queryByText("Repeated context")).toBeNull();
});
