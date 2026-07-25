export type MarkdownPhase = "streaming" | "complete";

export type ProjectedMarkdownBlockKind = "markdown" | "code" | "mermaid";

export interface MarkdownRepairDiagnostic {
  readonly kind: "attached_closing_fence";
  readonly sourceStart: number;
  readonly sourceEnd: number;
}

export interface ProjectedMarkdownBlock {
  readonly key: string;
  readonly source: string;
  readonly sourceStart: number;
  readonly sourceEnd: number;
  readonly stability: "stable" | "live";
  readonly kind: ProjectedMarkdownBlockKind;
  readonly syntheticClosingFence: boolean;
}

export interface MarkdownProjection {
  readonly mode: MarkdownPhase;
  readonly sourceLength: number;
  readonly source: string;
  readonly blocks: readonly ProjectedMarkdownBlock[];
  readonly diagnostics: readonly MarkdownRepairDiagnostic[];
}

export interface MarkdownRenderIdentity {
  readonly contentId: string;
  readonly phase: MarkdownPhase;
}
