import { render, screen } from "@testing-library/react";
import { createElement } from "react";
import { describe, expect, it, vi } from "vitest";

import { HistoryContent } from "../../HistoryPanel";
import { ToolCard } from "../../ToolCard";
import { isUnifiedDiff } from "../../DiffViewer";
import { catalogFixtures, UI_CATALOG_MARKER } from "./fixtures";

describe("desktop UI catalog contract", () => {
  it("covers every theme and adaptive viewport without entering production", () => {
    expect(UI_CATALOG_MARKER).toBe("sigil-desktop-dev-ui-catalog");
    expect(catalogFixtures.map((fixture) => fixture.id)).toEqual([
      "no-workspace",
      "empty-catalog",
      "session-catalog-30",
      "session-catalog-100",
      "degraded-catalog",
      "running-tool-approval",
      "reconnect-gap",
      "verification-failed-diff",
      "long-copy",
      "missing-optional-metadata",
    ]);

    for (const fixture of catalogFixtures) {
      expect(fixture.themes).toEqual(["system", "light", "dark"]);
      expect(fixture.viewports).toEqual([1280, 900, 840, 839, 760, 320]);
      expect(fixture.contrastModes).toEqual(["normal", "forced-colors"]);
      expect(fixture.motionModes).toEqual(["full", "reduced"]);
      expect(fixture.zoomFactors).toEqual([1, 2]);
    }
  });

  it("provides a real thirty-row density fixture with whole-row session actions", () => {
    const fixture = catalogFixtures.find(({ id }) => id === "session-catalog-30");
    expect(fixture?.sessions).toHaveLength(30);
    expect(fixture?.minimumFullyVisibleRows1280x720).toBe(5);

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
  });

  it("carries the complete adaptive domain-state evidence matrix", () => {
    const fixtures = new Map(catalogFixtures.map((fixture) => [fixture.id, fixture]));
    expect(fixtures.get("empty-catalog")?.sessions).toEqual([]);
    expect(fixtures.get("session-catalog-100")?.sessions).toHaveLength(100);
    expect(fixtures.get("degraded-catalog")?.degradedCounts).toEqual({ unavailable: 2, changed: 1, truncated: 1 });
    expect(fixtures.get("running-tool-approval")?.streamState).toBe("live");
    expect(fixtures.get("running-tool-approval")?.approval?.risk).toBe("high");
    expect(fixtures.get("running-tool-approval")?.tool?.duration).toBe("184 ms");
    expect(fixtures.get("reconnect-gap")?.streamState).toBe("reconnecting");
    expect(fixtures.get("reconnect-gap")?.attachmentGap).toBe(true);
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
    expect(screen.getByText("shell")).toBeTruthy();
  });
});
