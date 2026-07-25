import { isValidElement, useState, type ReactNode } from "react";

import { writeClipboard } from "../clipboard";
import { useLocale } from "../i18n";
import { Icon } from "../ui/icons";
import { IconButton, Tooltip } from "../ui/primitives";

export function MarkdownCodeBlock({
  children,
  variant,
  ariaLabel,
}: {
  readonly children: ReactNode;
  readonly variant: "message" | "embedded";
  readonly ariaLabel?: string;
}) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  const text = reactNodeText(children).replace(/\n$/, "");
  if (variant === "embedded") {
    return <pre className="tool-output syntax-highlight" aria-label={ariaLabel}>{children}</pre>;
  }
  const language = codeLanguage(children) ?? t("code");
  return (
    <div className="code-block">
      <header>
        <span>{language}</span>
        <Tooltip label={copied ? t("copied") : t("copyCode")}>
          <IconButton
            className="inline-copy"
            type="button"
            onClick={() => void writeClipboard(text).then(setCopied)}
            aria-label={t("copyCode")}
            icon={<Icon name={copied ? "check" : "copy"} />}
          />
        </Tooltip>
      </header>
      <pre className="syntax-highlight">{children}</pre>
    </div>
  );
}

export function reactNodeText(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(reactNodeText).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return reactNodeText(node.props.children);
  return "";
}

function codeLanguage(children: ReactNode): string | undefined {
  const first = Array.isArray(children) ? children[0] : children;
  if (!isValidElement<{ className?: string }>(first)) return undefined;
  return first.props.className
    ?.split(/\s+/)
    .find((name) => name.startsWith("language-"))
    ?.slice("language-".length);
}
