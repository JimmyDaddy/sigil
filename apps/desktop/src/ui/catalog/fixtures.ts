export const UI_CATALOG_MARKER = "sigil-desktop-dev-ui-catalog";

export type CatalogTheme = "system" | "light" | "dark";
export type CatalogViewport = 1280 | 900 | 840 | 839 | 760 | 320;

export interface CatalogFixture {
  readonly id: string;
  readonly description: string;
  readonly themes: readonly CatalogTheme[];
  readonly viewports: readonly CatalogViewport[];
}

const allThemes = ["system", "light", "dark"] as const;
const allViewports = [1280, 900, 840, 839, 760, 320] as const;

export const catalogFixtures: readonly CatalogFixture[] = [
  {
    id: "no-workspace",
    description: "No workspace selected",
    themes: allThemes,
    viewports: allViewports,
  },
  {
    id: "session-catalog-30",
    description: "Thirty sessions with long English and Chinese titles",
    themes: allThemes,
    viewports: allViewports,
  },
  {
    id: "running-tool-approval",
    description: "Active run with a tool result and high-risk approval",
    themes: allThemes,
    viewports: allViewports,
  },
  {
    id: "verification-failed-diff",
    description: "Failed verification with diff and evidence context",
    themes: allThemes,
    viewports: allViewports,
  },
];
