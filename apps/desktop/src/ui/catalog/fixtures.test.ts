import { render, screen } from "@testing-library/react";
import { createElement } from "react";
import { describe, expect, it, vi } from "vitest";

import { HistoryContent } from "../../HistoryPanel";
import { catalogFixtures, UI_CATALOG_MARKER } from "./fixtures";

describe("desktop UI catalog contract", () => {
  it("covers every theme and adaptive viewport without entering production", () => {
    expect(UI_CATALOG_MARKER).toBe("sigil-desktop-dev-ui-catalog");
    expect(catalogFixtures.map((fixture) => fixture.id)).toEqual([
      "no-workspace",
      "session-catalog-30",
      "running-tool-approval",
      "verification-failed-diff",
    ]);

    for (const fixture of catalogFixtures) {
      expect(fixture.themes).toEqual(["system", "light", "dark"]);
      expect(fixture.viewports).toEqual([1280, 900, 840, 839, 760, 320]);
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
});
