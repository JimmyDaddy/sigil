import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { groupEntries, HistoryContent } from "../../HistoryPanel";
import { App } from "../../App";
import { ToolCard } from "../../ToolCard";
import { isUnifiedDiff } from "../../DiffViewer";
import { CatalogApp } from "./CatalogApp";
import { catalogFixtures, UI_CATALOG_MARKER } from "./fixtures";
import { createCatalogWorkbenchBridge } from "./workbenchBridge";

afterEach(cleanup);

describe("desktop UI catalog contract", () => {
  it("covers every theme and adaptive viewport without entering production", () => {
    expect(UI_CATALOG_MARKER).toBe("sigil-desktop-dev-ui-catalog");
    expect(catalogFixtures.map((fixture) => fixture.id)).toEqual([
      "no-workspace",
      "empty-catalog",
      "session-catalog-30",
      "session-catalog-100",
      "degraded-catalog",
      "workbench-complete",
      "running-tool-approval",
      "reconnect-gap",
      "coding-composer",
      "tool-error-raw-details",
      "verification-failed-diff",
      "long-copy",
      "missing-optional-metadata",
    ]);

    for (const fixture of catalogFixtures) {
      expect(fixture.themes).toEqual(["system", "light", "dark"]);
      expect(fixture.viewports).toEqual([1280, 1024, 900, 899, 760, 320]);
      expect(fixture.contrastModes).toEqual(["normal", "forced-colors"]);
      expect(fixture.motionModes).toEqual(["full", "reduced"]);
      expect(fixture.zoomFactors).toEqual([1, 2]);
    }
  });

  it("provides a real thirty-row density fixture with whole-row session actions", () => {
    const fixture = catalogFixtures.find(({ id }) => id === "session-catalog-30");
    expect(fixture?.sessions).toHaveLength(30);
    expect(fixture?.minimumFullyVisibleRows1280x720).toBe(8);

    render(
      createElement(HistoryContent, {
        state: "ready",
        page: {
          workspaceId: "catalog-workspace",
          generation: 1,
          reconciledAtUnixMs: 1_784_419_200_000,
          degradedSourceCount: 0,
          identityConflictCount: 0,
          truncatedSourceCount: 0,
          entries: [...(fixture?.sessions ?? [])],
        },
        onRetry: vi.fn(),
        onLoadMore: vi.fn(),
        onOpen: vi.fn(),
      }),
    );

    expect(screen.getAllByRole("button")).toHaveLength(30);
    expect(screen.queryByRole("button", { name: "Open" })).toBeNull();
    expect(screen.getByRole("heading", { name: "Today" })).toBeTruthy();
    expect(screen.queryByText("deepseek-chat")).toBeNull();
  });

  it("groups session navigation by local calendar day without reordering entries", () => {
    const reference = new Date(2026, 6, 20, 15, 0).getTime();
    const entries = [
      { ...catalogFixtures.find(({ id }) => id === "session-catalog-30")!.sessions![0]!, sessionId: "today", sourceModifiedAtUnixMs: new Date(2026, 6, 20, 9, 0).getTime() },
      { ...catalogFixtures.find(({ id }) => id === "session-catalog-30")!.sessions![1]!, sessionId: "yesterday", sourceModifiedAtUnixMs: new Date(2026, 6, 19, 18, 0).getTime() },
      { ...catalogFixtures.find(({ id }) => id === "session-catalog-30")!.sessions![2]!, sessionId: "earlier", sourceModifiedAtUnixMs: new Date(2026, 6, 17, 18, 0).getTime() },
    ];

    expect(groupEntries(entries, reference).map((group) => [group.label, group.entries.map((entry) => entry.sessionId)])).toEqual([
      ["Today", ["today"]],
      ["Yesterday", ["yesterday"]],
      ["Earlier", ["earlier"]],
    ]);
  });

  it("keeps degraded source detail behind a compact diagnostic disclosure", () => {
    const fixture = catalogFixtures.find(({ id }) => id === "degraded-catalog")!;
    render(
      createElement(HistoryContent, {
        state: "ready",
        page: {
          workspaceId: "catalog-workspace",
          generation: 1,
          reconciledAtUnixMs: 1_784_419_200_000,
          degradedSourceCount: fixture.degradedCounts!.unavailable,
          identityConflictCount: fixture.degradedCounts!.changed,
          truncatedSourceCount: fixture.degradedCounts!.truncated,
          entries: [...fixture.sessions!],
        },
        onRetry: vi.fn(),
        onLoadMore: vi.fn(),
        onOpen: vi.fn(),
      }),
    );

    expect(screen.queryByText("Some sources need attention")).toBeNull();
    const disclosure = screen.getByRole("button", { name: "Catalog diagnostics, 4 issues" });
    fireEvent.click(disclosure);
    expect(screen.getByRole("dialog", { name: "Catalog diagnostics, 4 issues" })).toBeTruthy();
    expect(screen.getByText("Too large to inspect")).toBeTruthy();
  });

  it("carries the complete adaptive domain-state evidence matrix", () => {
    const fixtures = new Map(catalogFixtures.map((fixture) => [fixture.id, fixture]));
    expect(fixtures.get("empty-catalog")?.sessions).toEqual([]);
    expect(fixtures.get("session-catalog-100")?.sessions).toHaveLength(100);
    expect(fixtures.get("degraded-catalog")?.degradedCounts).toEqual({ unavailable: 2, changed: 1, truncated: 1 });
    expect(fixtures.get("workbench-complete")?.fullWorkbench).toBe(true);
    expect(fixtures.get("running-tool-approval")?.streamState).toBe("live");
    expect(fixtures.get("running-tool-approval")?.approval?.risk).toBe("high");
    expect(fixtures.get("running-tool-approval")?.tool?.duration).toBe("184 ms");
    expect(fixtures.get("reconnect-gap")?.streamState).toBe("reconnecting");
    expect(fixtures.get("reconnect-gap")?.attachmentGap).toBe(true);
    expect(fixtures.get("coding-composer")?.composer?.context.modelName).toBe("deepseek-v4-flash");
    expect(fixtures.get("tool-error-raw-details")?.tool?.status).toBe("error");
    expect(fixtures.get("verification-failed-diff")?.verification?.verdict).toBe("failed");
    expect(isUnifiedDiff(fixtures.get("verification-failed-diff")?.diff ?? "")).toBe(true);
    expect(fixtures.get("long-copy")?.longCopy).toMatch(/Investigate.*检查/);
    const minimalTool = fixtures.get("missing-optional-metadata")?.tool;
    expect(minimalTool?.toolName).toBe("shell");
    expect(minimalTool?.duration).toBeUndefined();
    expect(minimalTool?.risk).toBeUndefined();
  });

  it("renders missing optional tool metadata without placeholder noise", () => {
    const tool = catalogFixtures.find(({ id }) => id === "missing-optional-metadata")?.tool;
    expect(tool).toBeDefined();
    render(createElement(ToolCard, { tool: tool! }));
    expect(screen.queryByText(/duration not recorded|risk not classified/i)).toBeNull();
    expect(screen.getByText("Shell")).toBeTruthy();
  });

  it("provides a runnable catalog surface for theme and viewport inspection", () => {
    render(createElement(CatalogApp));
    expect(screen.getByRole("heading", { name: "Sigil UI catalog" })).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Theme"), { target: { value: "light" } });
    fireEvent.change(screen.getByLabelText("Viewport"), { target: { value: "320" } });
    fireEvent.change(screen.getByLabelText("Zoom"), { target: { value: "2" } });
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(screen.getByText(/320px viewport · 200% zoom/)).toBeTruthy();
  });

  it("renders the real application workbench from a capability-free fixture bridge", async () => {
    const user = userEvent.setup();
    render(createElement(App, { bridge: createCatalogWorkbenchBridge("dark") }));

    expect(await screen.findByRole("button", { name: /Review parser recovery and verification/ })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: /Review parser recovery and verification/ }));

    expect(await screen.findByRole("heading", { name: "Review parser recovery and verification" })).toBeTruthy();
    expect(screen.getByText("deepseek-v4-flash")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Stop run" })).toBeTruthy();
    await user.click(await screen.findByRole("button", { name: "Open verification: check failed" }));
    expect(screen.getByRole("button", { name: "Retry check" }).hasAttribute("disabled")).toBe(true);
    await waitFor(() => expect(document.querySelectorAll(".app-shell")).toHaveLength(1));
  });

  it("uses a real nested viewport for complete workbench media queries", () => {
    render(createElement(CatalogApp));
    fireEvent.change(screen.getByLabelText("Fixture"), {
      target: { value: "workbench-complete" },
    });
    fireEvent.change(screen.getByLabelText("Theme"), { target: { value: "light" } });

    const frame = screen.getByTitle("Complete Sigil workbench fixture") as HTMLIFrameElement;
    expect(frame.src).toContain("/catalog-workbench.html?theme=light");
    expect(frame.closest("[data-fixture]")?.getAttribute("data-fixture")).toBe("workbench-complete");
  });
});
