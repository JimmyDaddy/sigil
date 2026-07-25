import { MarkdownRenderer, safeHttpsUrl } from "./markdown/MarkdownRenderer";
import { normalizeCompletedMarkdown } from "./markdown/normalize";
import type { MarkdownPhase } from "./markdown/types";

interface SafeMarkdownProps {
  readonly text: string;
  readonly phase?: MarkdownPhase;
  readonly contentId?: string;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
  readonly codeBlockVariant?: "message" | "embedded";
  readonly codeBlockAriaLabel?: string;
}

export function SafeMarkdown({
  text,
  phase = "complete",
  contentId = "static-markdown",
  onOpenExternalUrl,
  codeBlockVariant = "message",
  codeBlockAriaLabel,
}: SafeMarkdownProps) {
  return (
    <MarkdownRenderer
      text={text}
      phase={phase}
      contentId={contentId}
      onOpenExternalUrl={onOpenExternalUrl}
      codeBlockVariant={codeBlockVariant}
      codeBlockAriaLabel={codeBlockAriaLabel}
    />
  );
}

export function normalizeMarkdownFences(text: string): string {
  return normalizeCompletedMarkdown(text).source;
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
      contentId={`highlighted-code-${language ?? "plain"}`}
      codeBlockVariant="embedded"
      codeBlockAriaLabel={ariaLabel}
    />
  );
}

export { safeHttpsUrl };

function longestBacktickRun(text: string): number {
  let longest = 0;
  for (const match of text.matchAll(/`+/g)) longest = Math.max(longest, match[0].length);
  return longest;
}
