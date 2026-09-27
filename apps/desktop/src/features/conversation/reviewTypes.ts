export type ReviewDiffSide = "old" | "new";
export type ReviewFileState = "current" | "changed" | "unknown";

export interface ReviewDiffLine {
  text: string;
  oldLine?: number;
  newLine?: number;
}

export interface ReviewDiff {
  sourceCallId: string;
  diffDigest: string;
  path: string;
  lines: ReviewDiffLine[];
  fileState: ReviewFileState;
  truncated: boolean;
}

export interface CheckpointReview {
  checkpointId: string;
  checkpointDigest: string;
  diffs: ReviewDiff[];
  truncated: boolean;
}

export interface ReviewAnnotation {
  checkpointId: string;
  checkpointDigest: string;
  sourceCallId: string;
  diffDigest: string;
  path: string;
  side: ReviewDiffSide;
  startLine: number;
  endLine: number;
  comment: string;
}

export interface ChangeReviewDraft {
  annotations: Array<ReviewAnnotation & { draftId: number }>;
  selection?: { diffDigest: string; side: ReviewDiffSide; startLine: number; endLine: number };
  comment: string;
  nextId: number;
}

export function emptyChangeReviewDraft(): ChangeReviewDraft {
  return { annotations: [], comment: "", nextId: 1 };
}
