import type { CatalogEntry } from "../../types";

export const UI_CATALOG_MARKER = "sigil-desktop-dev-ui-catalog";

export type CatalogTheme = "system" | "light" | "dark";
export type CatalogViewport = 1280 | 900 | 840 | 839 | 760 | 320;

export interface CatalogFixture {
  readonly id: string;
  readonly description: string;
  readonly themes: readonly CatalogTheme[];
  readonly viewports: readonly CatalogViewport[];
  readonly sessions?: readonly CatalogEntry[];
  readonly minimumFullyVisibleRows1280x720?: number;
}

const allThemes = ["system", "light", "dark"] as const;
const allViewports = [1280, 900, 840, 839, 760, 320] as const;

function sessionEntries(count: number): CatalogEntry[] {
  return Array.from({ length: count }, (_, index) => ({
    sessionRef: `catalog-session-${index + 1}.jsonl`,
    sessionId: `catalog-session-${index + 1}`,
    sourceState: "ready",
    sourceModifiedAtUnixMs: 1_784_419_200_000 - index * 60_000,
    providerName: index % 2 === 0 ? "deepseek" : "openai",
    modelName: index % 2 === 0 ? "deepseek-chat" : "gpt-5",
    title: index % 3 === 0
      ? `Investigate a long-running workspace regression ${index + 1}`
      : `检查桌面端会话密度与键盘导航 ${index + 1}`,
    userMessageCount: index + 1,
    assistantMessageCount: index,
    toolResultCount: index % 4,
    pinned: index < 2,
  }));
}

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
    sessions: sessionEntries(30),
    minimumFullyVisibleRows1280x720: 5,
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
