import { useState } from "react";

import { DiffViewer, isUnifiedDiff } from "./DiffViewer";
import { HighlightedCode } from "./SafeMarkdown";
import { Icon, type IconName } from "./ui/icons";
import { Button } from "./ui/primitives";

const MAX_VISIBLE_OUTPUT_LINES = 240;
const OUTPUT_PREVIEW_LINES = 3;
const OUTPUT_PREVIEW_CHARACTERS = 480;
const MAX_SUMMARY_CHARACTERS = 280;

export interface ToolView {
  key: string;
  toolName: string;
  text: string;
  input?: string;
  status?: string;
  risk?: string;
  duration?: string;
}

type ToolTone = "neutral" | "info" | "success" | "warning" | "danger";

interface ToolPresentation {
  readonly displayName: string;
  readonly status: string;
  readonly tone: ToolTone;
  readonly summary?: string;
  readonly input?: string;
  readonly detailLabel?: string;
  readonly detailKind?: "diff" | "output" | "raw";
  readonly detailText?: string;
  readonly detailLanguage?: string;
}

interface StructuredOutput {
  readonly status?: string;
  readonly summary?: string;
  readonly input?: string;
  readonly output?: string;
  readonly language?: string;
  readonly hasError: boolean;
}

interface OutputPresentation {
  readonly summary: string;
  readonly detailText?: string;
  readonly language?: string;
}

interface OutputPreview {
  readonly text: string;
  readonly truncated: boolean;
}

export function ToolCard({
  tool,
  displayId,
}: {
  readonly tool: ToolView;
  readonly displayId?: string;
}) {
  const presentation = presentTool(tool);
  const boundedDetail = boundedOutput(presentation.detailText ?? "");
  const inputLanguage = toolInputLanguage(tool.toolName);
  return (
    <article
      className={`tool-card tool-tone-${presentation.tone}${presentation.detailKind === undefined ? " tool-card-compact" : ""}`}
      data-display-id={displayId}
      aria-label={`${presentation.displayName}: ${presentation.status}`}
    >
      <header className="tool-card-header">
        <span className="tool-status-icon" aria-hidden="true"><Icon name={toneIcon(presentation.tone)} /></span>
        <span className="tool-card-heading">
          <strong>{presentation.displayName}</strong>
          {presentation.tone === "success" ? null : <small>{presentation.status}</small>}
        </span>
        <span className="tool-card-meta">
          {tool.duration === undefined ? null : <small>{tool.duration}</small>}
          {tool.risk === undefined ? null : <small className="tool-risk">{tool.risk} risk</small>}
        </span>
      </header>
      {presentation.input === undefined && presentation.summary === undefined ? null : (
        <div className="tool-card-main">
          {presentation.input === undefined ? null : (
            <div className={`tool-input${inputLanguage === undefined ? " is-parameters" : " is-command"}`}>
              <HighlightedCode text={presentation.input} language={inputLanguage} ariaLabel={`${tool.toolName} input`} />
            </div>
          )}
          {presentation.summary === undefined ? null : (
            <div className="tool-result">
              <small>{presentation.tone === "danger" || presentation.tone === "warning" ? "Status" : "Result"}</small>
              <p>{presentation.summary}</p>
            </div>
          )}
        </div>
      )}
      {presentation.detailKind === "output" ? (
        <ToolOutputPanel
          key={tool.key}
          toolName={tool.toolName}
          text={presentation.detailText ?? ""}
          language={presentation.detailLanguage}
        />
      ) : presentation.detailKind === undefined ? null : (
        <details className="tool-details">
          <summary>
            <span>{presentation.detailLabel}</span>
            <small>{detailMetadata(
              presentation.detailLanguage,
              presentation.detailText?.split("\n").length ?? 0,
            )}</small>
          </summary>
          <div className="tool-card-body">
            {presentation.detailKind === "diff" ? (
              <DiffViewer diff={presentation.detailText ?? ""} />
            ) : (
              <HighlightedCode
                text={boundedDetail.text}
                language={presentation.detailLanguage}
                ariaLabel={`${tool.toolName} ${presentation.detailKind === "raw" ? "raw details" : "output"}`}
              />
            )}
            {boundedDetail.omittedLines > 0 ? (
              <small>
                {boundedDetail.omittedLines} output line
                {boundedDetail.omittedLines === 1 ? "" : "s"} omitted from this view.
              </small>
            ) : null}
          </div>
        </details>
      )}
    </article>
  );
}

function ToolOutputPanel({
  toolName,
  text,
  language,
}: {
  readonly toolName: string;
  readonly text: string;
  readonly language?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const bounded = boundedOutput(text);
  const preview = outputPreview(bounded.text);
  const structured = language === "json";
  const expandable = structured || preview.truncated;
  const visibleText = expanded || !expandable ? bounded.text : preview.text;
  const showOutput = expanded || !structured;
  const lineCount = text.split("\n").length;
  const metadata = detailMetadata(language, lineCount);
  return (
    <section
      className={`tool-output-section${expanded ? " is-expanded" : ""}`}
      aria-label={`${toolName} output`}
    >
      {expandable ? (
        <Button
          type="button"
          variant="quiet"
          className="tool-output-disclosure"
          aria-label={`${expanded ? "Collapse" : "Expand"} ${toolName} output`}
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          <span className="tool-output-disclosure-title">
            <Icon name={expanded ? "chevron-up" : "chevron-down"} />
            <strong>Output</strong>
          </span>
          <small>{metadata}</small>
        </Button>
      ) : (
        <header>
          <strong>Output</strong>
          <small>{metadata}</small>
        </header>
      )}
      {showOutput ? (
        <HighlightedCode
          text={visibleText}
          language={language}
          ariaLabel={`${toolName} output content`}
        />
      ) : null}
      {expanded && bounded.omittedLines > 0 ? (
        <small>{bounded.omittedLines} output line{bounded.omittedLines === 1 ? "" : "s"} omitted from this view.</small>
      ) : null}
    </section>
  );
}

export function presentTool(tool: ToolView): ToolPresentation {
  const structured = parseStructuredOutput(tool.text);
  const status = structured?.status ?? tool.status ?? "recorded";
  const tone = statusTone(status);
  const diff = isUnifiedDiff(tool.text);
  const outputPresentation = diff
    ? undefined
    : presentOutput(
      structured === undefined ? tool.text : structured.output,
      tool.toolName,
      tone,
      structured?.language,
    );
  const summary = structured?.summary
    ?? (diff ? "File changes are ready to review." : outputPresentation?.summary);
  const detailKind = diff
    ? "diff" as const
    : structured?.hasError
      ? "raw" as const
      : outputPresentation?.detailText !== undefined
      ? "output" as const
      : undefined;
  return {
    displayName: humanizeToolName(tool.toolName),
    status: humanizeStatus(status),
    tone,
    summary,
    input: tool.input ?? structured?.input,
    detailKind,
    detailLabel: detailKind === "diff" ? "Review changes" : detailKind === "raw" ? "Raw details" : undefined,
    detailText: detailKind === "raw"
      ? tool.text
      : detailKind === undefined ? undefined : outputPresentation?.detailText ?? tool.text,
    detailLanguage: detailKind === "raw"
      ? "json"
      : detailKind === "output"
        ? outputPresentation?.language
        : undefined,
  };
}

function parseStructuredOutput(text: string): StructuredOutput | undefined {
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
  if (!("status" in root || "content" in root || "error" in root || "meta" in root)) {
    return undefined;
  }
  const error = asRecord(root.error);
  const meta = asRecord(root.meta);
  const details = asRecord(meta?.details);
  const call = asRecord(details?.call);
  const status = stringValue(root.status);
  const content = stringValue(root.content);
  const summary = firstText(
    stringValue(error?.message),
    stringValue(details?.summary),
  );
  return {
    status,
    summary: summary === undefined ? undefined : boundedSummary(summary),
    input: firstText(stringValue(call?.command), stringValue(call?.summary)),
    output: error === undefined ? content : undefined,
    language: inferOutputLanguage(
      stringValue(details?.language),
      firstText(stringValue(details?.path), stringValue(call?.path)),
      content,
    ),
    hasError: error !== undefined,
  };
}

function presentOutput(
  content: string | undefined,
  toolName: string,
  tone: ToolTone,
  explicitLanguage?: string,
): OutputPresentation | undefined {
  const trimmed = content?.trim() ?? "";
  if (trimmed === "") return fallbackSummary(tone);

  const json = parseJson(trimmed);
  if (json.parsed) {
    if (isEmptyJsonValue(json.value)) {
      return { summary: emptyOutputSummary(toolName) };
    }
    return {
      summary: jsonOutputSummary(json.value, toolName),
      detailText: trimmed,
      language: explicitLanguage ?? "json",
    };
  }

  const lines = trimmed.split("\n");
  const firstLine = lines.find((line) => line.trim() !== "")?.trim();
  const language = explicitLanguage ?? inferContentLanguage(content);
  const summary = firstLine === undefined
    ? fallbackSummary(tone)?.summary
    : isStructuralOnlyLine(firstLine)
      ? `${lines.length} lines of output.`
      : textOutputSummary(lines, toolName);
  if (summary === undefined) return undefined;
  return {
    summary,
    detailText: lines.length > 1 || language !== undefined ? trimmed : undefined,
    language,
  };
}

function parseJson(value: string): { readonly parsed: true; readonly value: unknown } | { readonly parsed: false } {
  if (!/^[\[{]/.test(value) && value !== "null") return { parsed: false };
  try {
    return { parsed: true, value: JSON.parse(value) as unknown };
  } catch {
    return { parsed: false };
  }
}

function isEmptyJsonValue(value: unknown): boolean {
  if (value === null) return true;
  if (typeof value === "string") return value.trim() === "";
  if (Array.isArray(value)) return value.length === 0;
  const record = asRecord(value);
  return record !== undefined && Object.keys(record).length === 0;
}

function emptyOutputSummary(toolName: string): string {
  const normalized = toolName.toLocaleLowerCase();
  if (/grep|search|find/.test(normalized)) return "No matches found.";
  if (/glob|list|(?:^|[_-])ls(?:$|[_-])|directory/.test(normalized)) return "No entries found.";
  return "Completed with no output.";
}

function jsonOutputSummary(value: unknown, toolName: string): string {
  if (Array.isArray(value)) {
    const matches = /grep|search|find/.test(toolName.toLocaleLowerCase());
    const noun = matches
      ? value.length === 1 ? "match" : "matches"
      : value.length === 1 ? "item" : "items";
    return `${value.length} ${noun}.`;
  }
  const record = asRecord(value);
  if (record !== undefined) {
    const fields = Object.keys(record).length;
    return `${fields} field${fields === 1 ? "" : "s"} of structured output.`;
  }
  return "Structured output available.";
}

function isStructuralOnlyLine(value: string): boolean {
  return /^[\[\]\{\},]+$/.test(value);
}

function textOutputSummary(lines: readonly string[], toolName: string): string {
  const normalized = toolName.toLocaleLowerCase();
  const lineCount = lines.length;
  if (/(?:^|[_-])(?:read|read_file|view_file)(?:$|[_-])/.test(normalized)) {
    return `${lineCount} line${lineCount === 1 ? "" : "s"} read.`;
  }
  if (lineCount > 1) return `${lineCount} lines of output.`;
  return boundedSummary(lines[0]?.trim() ?? "");
}

function boundedOutput(text: string): { readonly text: string; readonly omittedLines: number } {
  const lines = text.split("\n");
  return {
    text: lines.slice(0, MAX_VISIBLE_OUTPUT_LINES).join("\n"),
    omittedLines: Math.max(0, lines.length - MAX_VISIBLE_OUTPUT_LINES),
  };
}

function outputPreview(text: string): OutputPreview {
  const lines = text.split("\n");
  const linePreview = lines.slice(0, OUTPUT_PREVIEW_LINES).join("\n");
  if (lines.length > OUTPUT_PREVIEW_LINES) {
    return { text: linePreview, truncated: true };
  }
  if (linePreview.length > OUTPUT_PREVIEW_CHARACTERS) {
    return {
      text: `${linePreview.slice(0, OUTPUT_PREVIEW_CHARACTERS - 1).trimEnd()}…`,
      truncated: true,
    };
  }
  return { text: linePreview, truncated: false };
}

function toolInputLanguage(toolName: string): string | undefined {
  return /bash|shell|terminal/i.test(toolName) ? "bash" : undefined;
}

function inferOutputLanguage(
  explicit: string | undefined,
  path: string | undefined,
  content: string | undefined,
): string | undefined {
  return normalizeLanguage(explicit)
    ?? languageFromPath(path)
    ?? inferContentLanguage(content);
}

function inferContentLanguage(content: string | undefined): string | undefined {
  if (content === undefined) return undefined;
  const trimmed = content.trim();
  if (!((trimmed.startsWith("{") && trimmed.endsWith("}"))
    || (trimmed.startsWith("[") && trimmed.endsWith("]")))) return undefined;
  try {
    JSON.parse(trimmed);
    return "json";
  } catch {
    return undefined;
  }
}

function languageFromPath(path: string | undefined): string | undefined {
  const extension = path?.toLocaleLowerCase().match(/\.([a-z0-9]+)$/)?.[1];
  return normalizeLanguage(extension);
}

function normalizeLanguage(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const normalized = value.trim().toLocaleLowerCase();
  const aliases: Readonly<Record<string, string>> = {
    c: "c",
    cc: "cpp",
    cpp: "cpp",
    cs: "csharp",
    css: "css",
    diff: "diff",
    go: "go",
    h: "c",
    hpp: "cpp",
    html: "xml",
    java: "java",
    js: "javascript",
    javascript: "javascript",
    json: "json",
    jsx: "javascript",
    kt: "kotlin",
    kotlin: "kotlin",
    lua: "lua",
    md: "markdown",
    markdown: "markdown",
    php: "php",
    py: "python",
    python: "python",
    rb: "ruby",
    rs: "rust",
    rust: "rust",
    sh: "bash",
    shell: "bash",
    sql: "sql",
    swift: "swift",
    toml: "ini",
    ts: "typescript",
    tsx: "typescript",
    typescript: "typescript",
    xml: "xml",
    yaml: "yaml",
    yml: "yaml",
  };
  return aliases[normalized];
}

function detailMetadata(language: string | undefined, lineCount: number): string {
  const lines = `${lineCount} line${lineCount === 1 ? "" : "s"}`;
  return language === undefined ? lines : `${humanizeStatus(language)} · ${lines}`;
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

function fallbackSummary(tone: ToolTone): OutputPresentation | undefined {
  switch (tone) {
    case "danger": return { summary: "The tool did not complete successfully." };
    case "warning": return { summary: "The tool needs attention." };
    case "success":
    case "info":
    case "neutral":
      return undefined;
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
  if (/^ok$|success|succeeded|complete|completed|ready|finished|passed/.test(status)) return "success";
  if (/running|progress|pending|starting|waiting/.test(status)) return "info";
  return "neutral";
}

function toneIcon(tone: ToolTone): IconName {
  if (tone === "success") return "check";
  if (tone === "warning" || tone === "danger") return "warning";
  return "more";
}
