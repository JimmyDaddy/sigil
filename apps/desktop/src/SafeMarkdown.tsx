import { isValidElement, useState, type MouseEvent, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import remarkGfm from "remark-gfm";

import { writeClipboard } from "./clipboard";
import { useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { IconButton, Tooltip } from "./ui/primitives";

const MAX_EXTERNAL_URL_BYTES = 2_048;
const ALLOWED_ELEMENTS = [
  "a", "blockquote", "br", "code", "del", "em", "h1", "h2", "h3", "h4", "h5", "h6",
  "hr", "input", "li", "ol", "p", "pre", "span", "strong", "table", "tbody", "td", "th",
  "thead", "tr", "ul",
];

interface SafeMarkdownProps {
  readonly text: string;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
  readonly codeBlockVariant?: "message" | "embedded";
  readonly codeBlockAriaLabel?: string;
}

export function SafeMarkdown({
  text,
  onOpenExternalUrl,
  codeBlockVariant = "message",
  codeBlockAriaLabel,
}: SafeMarkdownProps) {
  const components: Components = {
    a: ({ href, children }) => (
      <SafeExternalLink href={href} onOpenExternalUrl={onOpenExternalUrl}>
        {children}
      </SafeExternalLink>
    ),
    pre: ({ children }) => codeBlockVariant === "embedded"
      ? <pre className="tool-output syntax-highlight" aria-label={codeBlockAriaLabel}>{children}</pre>
      : <MarkdownCodeBlock>{children}</MarkdownCodeBlock>,
    code: ({ className, children }) => <code className={className}>{children}</code>,
    table: ({ children }) => <div className="markdown-table-scroll"><table>{children}</table></div>,
  };

  return (
    <ReactMarkdown
      allowedElements={ALLOWED_ELEMENTS}
      components={components}
      rehypePlugins={[[rehypeHighlight, { detect: false, plainText: ["text", "txt", "plain"] }]]}
      remarkPlugins={[remarkGfm]}
      skipHtml={false}
      unwrapDisallowed={false}
      urlTransform={(url) => safeHttpsUrl(url) ?? ""}
    >
      {text}
    </ReactMarkdown>
  );
}

export function HighlightedCode({
  text,
  language,
  ariaLabel,
}: {
  readonly text: string;
  readonly language?: string;
  readonly ariaLabel: string;
}) {
  const fence = "`".repeat(Math.max(3, longestBacktickRun(text) + 1));
  const info = language?.match(/^[a-z0-9-]{1,32}$/)?.[0] ?? "";
  return (
    <SafeMarkdown
      text={`${fence}${info}\n${text}\n${fence}`}
      codeBlockVariant="embedded"
      codeBlockAriaLabel={ariaLabel}
    />
  );
}

function SafeExternalLink({
  href,
  children,
  onOpenExternalUrl,
}: {
  readonly href?: string;
  readonly children: ReactNode;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
}) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  const admitted = safeHttpsUrl(href);
  if (admitted === undefined) return <span className="unsafe-link-text">{children}</span>;

  const open = async () => {
    if (onOpenExternalUrl !== undefined) {
      try {
        await onOpenExternalUrl(admitted);
        return;
      } catch {
        // The native route is best effort. A failed route falls back to an explicit copy.
      }
    }
    setCopied(await writeClipboard(admitted));
  };
  const preventSecondaryNavigation = (event: MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
  };

  return (
    <a
      className="safe-external-link"
      href={admitted}
      rel="noreferrer noopener"
      target="_blank"
      title={copied ? t("linkCopied") : t("openExternalLink")}
      draggable={false}
      onClick={(event) => {
        event.preventDefault();
        void open();
      }}
      onAuxClick={preventSecondaryNavigation}
      onContextMenu={preventSecondaryNavigation}
    >
      {children}
    </a>
  );
}

function MarkdownCodeBlock({ children }: { readonly children: ReactNode }) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  const text = reactNodeText(children).replace(/\n$/, "");
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

export function safeHttpsUrl(candidate: string | undefined): string | undefined {
  if (candidate === undefined || candidate.length > MAX_EXTERNAL_URL_BYTES) return undefined;
  try {
    const parsed = new URL(candidate);
    if (
      parsed.protocol !== "https:"
      || parsed.hostname === ""
      || parsed.username !== ""
      || parsed.password !== ""
    ) return undefined;
    return parsed.href;
  } catch {
    return undefined;
  }
}

function codeLanguage(children: ReactNode): string | undefined {
  const first = Array.isArray(children) ? children[0] : children;
  if (!isValidElement<{ className?: string }>(first)) return undefined;
  return first.props.className
    ?.split(/\s+/)
    .find((name) => name.startsWith("language-"))
    ?.slice("language-".length);
}

function reactNodeText(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(reactNodeText).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return reactNodeText(node.props.children);
  return "";
}

function longestBacktickRun(text: string): number {
  let longest = 0;
  for (const match of text.matchAll(/`+/g)) longest = Math.max(longest, match[0].length);
  return longest;
}
