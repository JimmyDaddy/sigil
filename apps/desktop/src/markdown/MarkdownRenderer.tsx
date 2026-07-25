import {
  createContext,
  lazy,
  Suspense,
  useContext,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
  type ReactNode,
} from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";

import { writeClipboard } from "../clipboard";
import { useLocale } from "../i18n";
import { ReadonlyCheckbox } from "../ui/primitives";
import { MarkdownCodeBlock, reactNodeText } from "./MarkdownCodeBlock";
import { MermaidDiagram } from "./MermaidDiagram";
import { MAX_MERMAID_DIAGRAMS_PER_MESSAGE } from "./mermaidSecurity";
import {
  projectMarkdownWithCursor,
  type MarkdownProjectionCursor,
} from "./projection";
import type { MarkdownPhase } from "./types";

const MAX_EXTERNAL_URL_BYTES = 2_048;
const MathMarkdownDocument = lazy(async () => {
  const module = await import("./MarkdownMath");
  return { default: module.MarkdownMath };
});

const SAFE_SCHEMA = {
  ...defaultSchema,
  tagNames: [
    "a", "blockquote", "br", "code", "del", "em", "h1", "h2", "h3", "h4", "h5", "h6",
    "hr", "input", "li", "ol", "p", "pre", "span", "strong", "table", "tbody", "td", "th",
    "thead", "tr", "ul",
  ],
  attributes: {
    ...defaultSchema.attributes,
    code: [
      ...(defaultSchema.attributes?.code ?? []),
      ["className", /^language-[A-Za-z0-9_-]{1,32}$/u],
    ],
    span: [
      ...(defaultSchema.attributes?.span ?? []),
      ["className", "math-inline", "math-display"],
    ],
    input: ["type", "checked", "disabled"],
  },
  protocols: {
    ...defaultSchema.protocols,
    href: ["https"],
  },
};

export interface MarkdownRendererProps {
  readonly text: string;
  readonly phase: MarkdownPhase;
  readonly contentId: string;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
  readonly codeBlockVariant: "message" | "embedded";
  readonly codeBlockAriaLabel?: string;
}

export function MarkdownRenderer(props: MarkdownRendererProps) {
  const cursor = useRef<MarkdownProjectionCursor | undefined>(undefined);
  const update = useMemo(
    () => projectMarkdownWithCursor({
      source: props.text,
      phase: props.phase,
      contentId: props.contentId,
    }, cursor.current),
    [props.contentId, props.phase, props.text],
  );
  useLayoutEffect(() => {
    cursor.current = update.cursor;
  }, [update.cursor]);
  const projection = update.projection;

  if (props.phase === "complete") {
    return (
      <MarkdownDocument
        {...props}
        source={projection.source}
        allowMermaid
      />
    );
  }

  let diagramCount = 0;
  return (
    <>
      {projection.blocks.map((block) => {
        if (block.kind === "mermaid") diagramCount += 1;
        return (
          <MarkdownDocument
            {...props}
            key={block.key}
            source={block.source}
            allowMermaid={
              block.kind === "mermaid"
              && block.stability === "stable"
              && diagramCount <= MAX_MERMAID_DIAGRAMS_PER_MESSAGE
            }
          />
        );
      })}
    </>
  );
}

export interface MarkdownDocumentProps extends Omit<MarkdownRendererProps, "text"> {
  readonly source: string;
  readonly allowMermaid: boolean;
}

export function MarkdownDocument(props: MarkdownDocumentProps) {
  const componentContext: MarkdownComponentContextValue = {
    props,
    diagramCount: 0,
  };
  if (hasMathCandidate(props.source)) {
    return (
      <MarkdownComponentContext.Provider value={componentContext}>
        <Suspense fallback={<PlainMarkdown {...props} />}>
          <MathMarkdownDocument {...props} components={MARKDOWN_COMPONENTS} schema={SAFE_SCHEMA} />
        </Suspense>
      </MarkdownComponentContext.Provider>
    );
  }
  return (
    <MarkdownComponentContext.Provider value={componentContext}>
      <PlainMarkdown {...props} />
    </MarkdownComponentContext.Provider>
  );
}

function PlainMarkdown({ source }: MarkdownDocumentProps) {
  return (
    <ReactMarkdown
      components={MARKDOWN_COMPONENTS}
      rehypePlugins={[
        [rehypeSanitize, SAFE_SCHEMA],
        [rehypeHighlight, { detect: false, plainText: ["text", "txt", "plain"] }],
      ]}
      remarkPlugins={[remarkGfm]}
      skipHtml
      unwrapDisallowed={false}
      urlTransform={(url) => safeHttpsUrl(url) ?? ""}
    >
      {source}
    </ReactMarkdown>
  );
}

interface MarkdownComponentContextValue {
  readonly props: MarkdownDocumentProps;
  diagramCount: number;
}

const MarkdownComponentContext = createContext<MarkdownComponentContextValue | undefined>(undefined);

function useMarkdownComponentContext(): MarkdownComponentContextValue {
  const context = useContext(MarkdownComponentContext);
  if (context === undefined) throw new Error("Markdown components require a document context");
  return context;
}

// Keep component types stable across parent rerenders. The document-scoped
// context owns the mutable admission counter, so a workspace health poll can
// rerender the conversation without remounting interactive diagram cards.
const MARKDOWN_COMPONENTS: Components = {
  a: function MarkdownLink({ href, children }) {
    const { props } = useMarkdownComponentContext();
    return (
      <SafeExternalLink href={href} onOpenExternalUrl={props.onOpenExternalUrl}>
        {children}
      </SafeExternalLink>
    );
  },
  pre: function MarkdownPre({ children }) {
    const context = useMarkdownComponentContext();
    const { props } = context;
    const language = codeLanguage(children);
    if (
      props.allowMermaid
      && language === "mermaid"
      && context.diagramCount < MAX_MERMAID_DIAGRAMS_PER_MESSAGE
    ) {
      context.diagramCount += 1;
      return (
        <MermaidDiagram
          source={reactNodeText(children).replace(/\n$/, "")}
          contentId={`${props.contentId}-${context.diagramCount}`}
        />
      );
    }
    return (
      <MarkdownCodeBlock
        variant={props.codeBlockVariant}
        ariaLabel={props.codeBlockAriaLabel}
      >
        {children}
      </MarkdownCodeBlock>
    );
  },
  code: function MarkdownCode({ className, children }) {
    return <code className={className}>{children}</code>;
  },
  table: function MarkdownTable({ children }) {
    return <div className="markdown-table-scroll"><table>{children}</table></div>;
  },
  input: function MarkdownInput({ checked }) {
    return <MarkdownTaskCheckbox checked={checked} />;
  },
};

function MarkdownTaskCheckbox({ checked }: { readonly checked?: boolean }) {
  const { t } = useLocale();
  return (
    <ReadonlyCheckbox
      checked={checked}
      aria-label={t(checked ? "completedTask" : "incompleteTask")}
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
  const element = Array.isArray(children) ? children[0] : children;
  if (
    typeof element !== "object"
    || element === null
    || !("props" in element)
  ) return undefined;
  const className = (element as { props?: { className?: unknown } }).props?.className;
  if (typeof className !== "string") return undefined;
  return className
    .split(/\s+/u)
    .find((name) => name.startsWith("language-"))
    ?.slice("language-".length)
    .toLowerCase();
}

export function hasMathCandidate(source: string): boolean {
  return /(^|[^\\])\$\$[\s\S]*?\$\$/u.test(source)
    || /(^|[^\\$])\$[^$\n]+\$/u.test(source);
}
