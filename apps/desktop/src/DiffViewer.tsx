import { useMemo, useState } from "react";

import { writeClipboard } from "./clipboard";

export function DiffViewer({ diff }: { diff: string }) {
  const [copied, setCopied] = useState(false);
  const lines = useMemo(() => diff.split("\n"), [diff]);
  return (
    <figure className="diff-viewer">
      <figcaption>
        <span>Proposed file changes</span>
        <button type="button" onClick={() => void writeClipboard(diff).then(setCopied)}>
          {copied ? "Copied" : "Copy diff"}
        </button>
      </figcaption>
      <pre aria-label="Unified diff">
        {lines.map((line, index) => <span className={`diff-${diffLineKind(line)}`} key={`${index}:${line}`}>{line || " "}</span>)}
      </pre>
    </figure>
  );
}

export function isUnifiedDiff(text: string): boolean {
  return /(^|\n)---\s/.test(text) && /(^|\n)\+\+\+\s/.test(text) && /(^|\n)@@\s/.test(text);
}

function diffLineKind(line: string): "add" | "remove" | "hunk" | "header" | "context" {
  if (line.startsWith("+++ ") || line.startsWith("--- ")) return "header";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "remove";
  if (line.startsWith("@@")) return "hunk";
  return "context";
}
