import { DiffViewer, isUnifiedDiff } from "./DiffViewer";
import { Icon, type IconName } from "./ui/icons";

const MAX_VISIBLE_OUTPUT_LINES = 240;
const MAX_SUMMARY_CHARACTERS = 280;

export interface ToolView {
  key: string;
  toolName: string;
  text: string;
  status?: string;
  risk?: string;
  duration?: string;
}

type ToolTone = "neutral" | "info" | "success" | "warning" | "danger";

interface ToolPresentation {
  readonly displayName: string;
  readonly status: string;
  readonly tone: ToolTone;
  readonly summary: string;
  readonly detailLabel?: string;
  readonly detailKind?: "diff" | "output" | "raw";
  readonly detailText?: string;
}

export function ToolCard({ tool }: { tool: ToolView }) {
  const presentation = presentTool(tool);
  const lines = presentation.detailText?.split("\n") ?? [];
  const output = lines.slice(0, MAX_VISIBLE_OUTPUT_LINES).join("\n");
  const omittedLines = Math.max(0, lines.length - MAX_VISIBLE_OUTPUT_LINES);
  return (
    <article className={`tool-card tool-tone-${presentation.tone}`} aria-label={`${presentation.displayName}: ${presentation.status}`}>
      <header className="tool-card-header">
        <span className="tool-status-icon" aria-hidden="true"><Icon name={toneIcon(presentation.tone)} /></span>
        <span className="tool-card-heading">
          <strong>{presentation.displayName}</strong>
          <small>{presentation.status}</small>
        </span>
        <span className="tool-card-meta">
          {tool.duration === undefined ? null : <small>{tool.duration}</small>}
          {tool.risk === undefined ? null : <small className="tool-risk">{tool.risk} risk</small>}
        </span>
      </header>
      <p className="tool-summary">{presentation.summary}</p>
      {presentation.detailKind === undefined ? null : (
        <details className="tool-details">
          <summary>
            <span>{presentation.detailLabel}</span>
            <small>{presentation.detailKind === "raw" ? "JSON" : `${lines.length} line${lines.length === 1 ? "" : "s"}`}</small>
          </summary>
          <div className="tool-card-body">
            {presentation.detailKind === "diff" ? (
              <DiffViewer diff={presentation.detailText ?? ""} />
            ) : (
              <pre className="tool-output" aria-label={`${tool.toolName} ${presentation.detailKind === "raw" ? "raw details" : "output"}`}>{output}</pre>
            )}
            {omittedLines > 0 ? <small>{omittedLines} output line{omittedLines === 1 ? "" : "s"} omitted from this view.</small> : null}
          </div>
        </details>
      )}
    </article>
  );
}

export function presentTool(tool: ToolView): ToolPresentation {
  const structured = parseStructuredOutput(tool.text);
  const status = structured?.status ?? tool.status ?? "recorded";
  const tone = statusTone(status);
  const trimmed = tool.text.trim();
  const diff = isUnifiedDiff(tool.text);
  const plainLines = trimmed === "" ? [] : trimmed.split("\n");
  const summary = structured?.summary
    ?? (diff ? "File changes are ready to review." : summarizePlainOutput(plainLines, tone));
  const detailKind = diff
    ? "diff" as const
    : structured !== undefined
      ? "raw" as const
      : plainLines.length > 1
        ? "output" as const
        : undefined;
  return {
    displayName: humanizeToolName(tool.toolName),
    status: humanizeStatus(status),
    tone,
    summary,
    detailKind,
    detailLabel: detailKind === "diff" ? "Review changes" : detailKind === "raw" ? "Raw details" : detailKind === "output" ? "View output" : undefined,
    detailText: detailKind === undefined ? undefined : tool.text,
  };
}

function parseStructuredOutput(text: string): { readonly status?: string; readonly summary: string } | undefined {
  const trimmed = text.trim();
  if (!trimmed.startsWith("{") || !trimmed.endsWith("}")) return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return undefined;
  }
  const root = asRecord(parsed);
  if (root === undefined) return undefined;
  const error = asRecord(root.error);
  const meta = asRecord(root.meta);
  const details = asRecord(meta?.details);
  const call = asRecord(details?.call);
  const status = stringValue(root.status);
  const summary = firstText(
    stringValue(error?.message),
    stringValue(root.content),
    stringValue(call?.summary),
    stringValue(details?.summary),
  ) ?? fallbackSummary(statusTone(status));
  return { status, summary: boundedSummary(summary) };
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : undefined;
}

function firstText(...values: Array<string | undefined>): string | undefined {
  return values.find((value) => value !== undefined);
}

function summarizePlainOutput(lines: readonly string[], tone: ToolTone): string {
  const firstLine = lines.find((line) => line.trim() !== "")?.trim();
  return firstLine === undefined ? fallbackSummary(tone) : boundedSummary(firstLine);
}

function fallbackSummary(tone: ToolTone): string {
  switch (tone) {
    case "success": return "Completed.";
    case "danger": return "The tool did not complete successfully.";
    case "warning": return "The tool needs attention.";
    case "info": return "The tool is running.";
    case "neutral": return "Tool activity was recorded.";
  }
}

function boundedSummary(value: string): string {
  if (value.length <= MAX_SUMMARY_CHARACTERS) return value;
  return `${value.slice(0, MAX_SUMMARY_CHARACTERS - 1).trimEnd()}…`;
}

function humanizeToolName(value: string): string {
  const words = value.replace(/[_-]+/g, " ").trim();
  if (words === "") return "Tool";
  return `${words.charAt(0).toLocaleUpperCase()}${words.slice(1)}`;
}

function humanizeStatus(value: string): string {
  const words = value.replace(/[_-]+/g, " ").trim();
  if (words === "") return "Recorded";
  return `${words.charAt(0).toLocaleUpperCase()}${words.slice(1)}`;
}

function statusTone(value?: string): ToolTone {
  const status = value?.toLocaleLowerCase() ?? "";
  if (/failed|failure|error|crash|invalid/.test(status)) return "danger";
  if (/denied|cancel|blocked|warning|expired/.test(status)) return "warning";
  if (/success|succeeded|complete|completed|ready|finished|passed/.test(status)) return "success";
  if (/running|progress|pending|starting|waiting/.test(status)) return "info";
  return "neutral";
}

function toneIcon(tone: ToolTone): IconName {
  if (tone === "success") return "check";
  if (tone === "warning" || tone === "danger") return "warning";
  return "more";
}
