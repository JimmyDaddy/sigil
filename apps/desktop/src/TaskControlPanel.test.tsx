import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { TaskControlPanel } from "./TaskControlPanel";

afterEach(cleanup);

it.each([undefined, "Implement the trip settlement tool"])("keeps a cancelled task title accurate (%s)", (objective) => {
  render(<TaskControlPanel task={{
    taskId: "task-cancelled", objective, status: "cancelled", steps: [], checklist: [],
    activeChildren: 0, completedChildren: 0, failedChildren: 0, lanes: [], canContinue: false,
  }} runActive={false} disabled={false} busy={false} reviewReady={false} reviewBusy={false}
    pauseDisabled={false} pauseBusy={false} onPause={vi.fn()} onContinue={vi.fn()} onReviewIntegration={vi.fn()} />);
  expect(screen.getByRole("heading", { name: objective ?? "Task" })).toBeTruthy();
  expect(screen.getByText("Cancelled")).toBeTruthy();
  expect(screen.queryByText(/Durable Task in progress/)).toBeNull();
  expect(screen.queryByRole("button", { name: "Continue Task" })).toBeNull();
});

it.each([false, true])("offers pause only for an exact active Task with a real running phase (owned=%s)", (owned) => {
  render(<TaskControlPanel task={{
    taskId: "task-exact", status: owned ? "started" : "paused", executionRunId: "task-run",
    execution: { kind: "direct", admissionId: "original-admission" }, steps: [], checklist: [],
    activeChildren: 0, completedChildren: 0, failedChildren: 0, lanes: [], canContinue: true,
  }} runActive={owned} disabled={!owned} busy={false} reviewReady={false} reviewBusy={false}
    pauseDisabled={false} pauseBusy={false} onPause={vi.fn()} onContinue={vi.fn()} onReviewIntegration={vi.fn()} />);
  if (owned) {
    expect(screen.getByRole("button", { name: "Pause to continue later" }).hasAttribute("disabled")).toBe(false);
    expect(screen.queryByText("Paused")).toBeNull();
    expect(screen.queryByRole("button", { name: "Continue Task" })).toBeNull();
  } else {
    expect(screen.queryByRole("button", { name: "Pause to continue later" })).toBeNull();
    expect(screen.getByText("Paused")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Continue Task" }).hasAttribute("disabled")).toBe(true);
  }
});
