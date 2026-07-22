import { useMemo, useState } from "react";

import { writeClipboard } from "./clipboard";
import { useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { IconButton, Tooltip } from "./ui/primitives";

export function DiffViewer({ diff }: { diff: string }) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  const lines = useMemo(() => diff.split("\n"), [diff]);
  return (
    <figure className="diff-viewer">
      <figcaption>
        <span>{t("proposedFileChanges")}</span>
        <Tooltip label={copied ? t("copied") : t("copyDiff")}>
          <IconButton
            className="diff-copy"
            type="button"
            onClick={() => void writeClipboard(diff).then(setCopied)}
            aria-label={t("copyDiff")}
            icon={<Icon name={copied ? "check" : "copy"} />}
          />
        </Tooltip>
      </figcaption>
      <pre aria-label={t("unifiedDiff")}>
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
