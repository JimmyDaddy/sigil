import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import type {
  MarkdownPhase,
  MarkdownRepairDiagnostic,
  ProjectedMarkdownBlock,
} from "../types";
import fixtureDocument from "../../../../../dev/fixtures/markdown-rendering-v1/cases.json";

interface FixtureBlock extends Omit<ProjectedMarkdownBlock, "key" | "source"> {}

export interface MarkdownFixtureCase {
  readonly id: string;
  readonly phase: MarkdownPhase;
  readonly sourceFile?: string;
  readonly source?: string;
  readonly sourceLength: number;
  readonly blocks: readonly FixtureBlock[];
  readonly diagnostics: readonly MarkdownRepairDiagnostic[];
}

interface FixtureDocument {
  readonly schemaVersion: number;
  readonly cases: readonly MarkdownFixtureCase[];
}

const document = fixtureDocument as FixtureDocument;
const fixtureRoot = resolve(process.cwd(), "../../dev/fixtures/markdown-rendering-v1");

export const markdownFixtureCases = document.cases;

export function fixtureSource(testCase: MarkdownFixtureCase): string {
  if (testCase.source !== undefined) return testCase.source;
  if (testCase.sourceFile === undefined) throw new Error(`fixture ${testCase.id} has no source`);
  return readFileSync(resolve(fixtureRoot, testCase.sourceFile), "utf8");
}
