export interface BranchLink {
  sessionRef: string;
  sessionId: string;
  title?: string;
  sourceTurnIndex: number;
  sourceTurnDigest: string;
}
export interface BranchLineage {
  sessionId: string;
  parent?: BranchLink | null;
  children: BranchLink[];
  unavailableCount: number;
}
export interface BranchKnowledgeSource {
  sourceSessionRef: string;
  sourceSessionId: string;
}
export interface BranchKnowledgePoint {
  sourceTurnDigest: string;
  sourceMessageId: string;
  sourceTextSha256: string;
  summarySha256: string;
  summary: string;
  truncated: boolean;
}
export interface BranchKnowledgePreview extends BranchKnowledgeSource {
  points: BranchKnowledgePoint[];
}
export interface BranchKnowledgeImport extends BranchKnowledgeSource {
  sourceTurnDigest: string;
  sourceMessageId: string;
  sourceTextSha256: string;
  summarySha256: string;
}
