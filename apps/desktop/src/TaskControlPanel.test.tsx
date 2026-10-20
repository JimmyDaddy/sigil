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
