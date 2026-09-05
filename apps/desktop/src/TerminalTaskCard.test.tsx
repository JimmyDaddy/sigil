import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LocaleProvider } from "./i18n";
import { TerminalTaskCard } from "./TerminalTaskCard";
import type { TimelineTerminalTask } from "./types";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

function task(overrides: Partial<TimelineTerminalTask> = {}): TimelineTerminalTask {
  return {
    taskId: "terminal-1",
    generation: 7,
    status: "running",
    readiness: "ready",
    totalOutputBytes: 42,
    emittedAtMs: 1,
    ...overrides,
  };
}

describe("TerminalTaskCard product status", () => {
  it("renders the mapped running state and keeps stop interaction intact", async () => {
    const user = userEvent.setup();
    const onStop = vi.fn();
    render(
      <LocaleProvider>
        <TerminalTaskCard task={task()} onStop={onStop} />
      </LocaleProvider>,
    );

    const card = screen.getByRole("article");
    expect(card.getAttribute("data-terminal-task-product-status")).toBe("running");
    expect(screen.getByText("Running")).toBeTruthy();
    expect(screen.getByText("Sigil is working on this operation.")).toBeTruthy();
    expect(screen.getByText(/Wait for progress or stop it if needed/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop task" })).toBeTruthy();
    expect(card.querySelector("header")?.textContent).not.toContain("terminal-1");

    await user.click(screen.getByRole("button", { name: "Stop task" }));
    expect(onStop).toHaveBeenCalledOnce();
  });

  it("does not present a process exit as Task success", () => {
    render(
      <LocaleProvider>
        <TerminalTaskCard task={task({ status: "exited", exitCode: 0 })} />
      </LocaleProvider>,
    );

    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("The terminal process exited.")).toBeTruthy();
    expect(screen.getByText(/process completion does not prove Task success/)).toBeTruthy();
    expect(screen.getByText("Show details")).toBeTruthy();
  });
});
