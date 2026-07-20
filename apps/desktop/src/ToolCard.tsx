import { DiffViewer, isUnifiedDiff } from "./DiffViewer";

const MAX_VISIBLE_OUTPUT_LINES = 240;

export interface ToolView {
  key: string;
  toolName: string;
  text: string;
  status?: string;
  risk?: string;
  duration?: string;
}

export function ToolCard({ tool }: { tool: ToolView }) {
  const lines = tool.text.split("\n");
  const output = lines.slice(0, MAX_VISIBLE_OUTPUT_LINES).join("\n");
  const omittedLines = Math.max(0, lines.length - MAX_VISIBLE_OUTPUT_LINES);
  return (
    <details className={`tool-card tool-${tool.status ?? "recorded"}`}>
      <summary>
        <span className="tool-name">{tool.toolName}</span>
        <span>{tool.status ?? "recorded"}</span>
        {tool.duration === undefined ? null : <span>{tool.duration}</span>}
        {tool.risk === undefined ? null : <span>{tool.risk}</span>}
      </summary>
      <div className="tool-card-body">
        {isUnifiedDiff(tool.text) ? (
          <DiffViewer diff={tool.text} />
        ) : (
          <pre className="tool-output" aria-label={`${tool.toolName} output`}>{output || "No output was recorded."}</pre>
        )}
        {omittedLines > 0 ? <small>{omittedLines} output line{omittedLines === 1 ? "" : "s"} omitted from this view.</small> : null}
      </div>
    </details>
  );
}
