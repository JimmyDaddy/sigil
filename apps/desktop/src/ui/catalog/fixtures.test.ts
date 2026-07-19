import { describe, expect, it } from "vitest";

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
});
