import ReactMarkdown, { type Components } from "react-markdown";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import rehypeSanitize from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";

import "katex/dist/katex.min.css";

import type { MarkdownDocumentProps } from "./MarkdownRenderer";

export function MarkdownMath({
  source,
  components,
  schema,
}: MarkdownDocumentProps & {
  readonly components: Components;
  readonly schema: Readonly<Record<string, unknown>>;
}) {
  return (
    <ReactMarkdown
      components={components}
      remarkPlugins={[remarkGfm, remarkMath]}
      rehypePlugins={[
        [rehypeSanitize, schema],
        [rehypeKatex, {
          output: "htmlAndMathml",
          throwOnError: false,
          trust: false,
          strict: "warn",
          maxExpand: 1_000,
          maxSize: 20,
          globalGroup: false,
        }],
        [rehypeHighlight, { detect: false, plainText: ["text", "txt", "plain"] }],
      ]}
      skipHtml
      unwrapDisallowed={false}
      urlTransform={(url) => url}
    >
      {source}
    </ReactMarkdown>
  );
}
