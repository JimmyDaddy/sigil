import { Fragment, useState } from "react";

import { writeClipboard } from "./clipboard";
import { useLocale } from "./i18n";
import { Icon } from "./ui/icons";
import { IconButton, Tooltip } from "./ui/primitives";

interface MessageContentProps {
  text: string;
}

type ContentBlock =
  | { kind: "code"; language?: string; text: string }
  | { kind: "heading"; level: 3 | 4 | 5; text: string }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "quote"; text: string }
  | { kind: "rule" }
  | { kind: "paragraph"; text: string };

export function MessageContent({ text }: MessageContentProps) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  const blocks = parseBlocks(text);
  if (blocks.length === 0) {
    return <p className="message-content-empty">{t("messageUnavailable")}</p>;
  }
  return (
    <div className="message-content">
      <Tooltip label={copied ? t("copied") : t("copyMessage")}>
        <IconButton
          className="content-copy"
          type="button"
          onClick={() => void writeClipboard(text).then(setCopied)}
          aria-label={t("copyMessage")}
          icon={<Icon name={copied ? "check" : "copy"} />}
        />
      </Tooltip>
      {blocks.map((block, index) => {
        const key = `${block.kind}:${index}`;
        if (block.kind === "code") {
          return (
            <div className="code-block" key={key}>
              <header><span>{block.language ?? t("code")}</span><CopyButton text={block.text} label={t("copyCode")} /></header>
              <pre><code>{block.text}</code></pre>
            </div>
          );
        }
        if (block.kind === "heading") {
          const Heading = `h${block.level}` as "h3" | "h4" | "h5";
          return <Heading key={key}>{inlineMarkup(block.text)}</Heading>;
        }
        if (block.kind === "quote") return <blockquote key={key}>{inlineMarkup(block.text)}</blockquote>;
        if (block.kind === "rule") return <hr key={key} />;
        if (block.kind === "list") {
          const List = block.ordered ? "ol" : "ul";
          return <List key={key}>{block.items.map((item, itemIndex) => <li key={`${itemIndex}:${item}`}>{inlineMarkup(item)}</li>)}</List>;
        }
        return <p key={key}>{inlineMarkup(block.text)}</p>;
      })}
    </div>
  );
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  return (
    <Tooltip label={copied ? t("copied") : label}>
      <IconButton
        className="inline-copy"
        type="button"
        onClick={() => void writeClipboard(text).then(setCopied)}
        aria-label={label}
        icon={<Icon name={copied ? "check" : "copy"} />}
      />
    </Tooltip>
  );
}

function inlineMarkup(text: string) {
  return text.split(/(`[^`]+`|\*\*[^*]+\*\*|__[^_]+__|\*[^*]+\*|_[^_]+_)/g).map((part, index) => {
    const key = `${index}:${part}`;
    if (part.startsWith("`") && part.endsWith("`")) return <code key={key}>{part.slice(1, -1)}</code>;
    if ((part.startsWith("**") && part.endsWith("**")) || (part.startsWith("__") && part.endsWith("__"))) {
      return <strong key={key}>{part.slice(2, -2)}</strong>;
    }
    if ((part.startsWith("*") && part.endsWith("*")) || (part.startsWith("_") && part.endsWith("_"))) {
      return <em key={key}>{part.slice(1, -1)}</em>;
    }
    return <Fragment key={key}>{part}</Fragment>;
  });
}

export function parseBlocks(text: string): ContentBlock[] {
  const lines = text.split("\r\n").join("\n").split("\n");
  const blocks: ContentBlock[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading !== null) {
      blocks.push({ kind: "heading", level: Math.min(5, heading[1].length + 2) as 3 | 4 | 5, text: heading[2] });
      index += 1;
      continue;
    }
    if (/^ {0,3}(?:-{3,}|\*{3,})\s*$/.test(line)) {
      blocks.push({ kind: "rule" });
      index += 1;
      continue;
    }
    if (line.startsWith("```")) {
      const language = line.slice(3).trim() || undefined;
      const code: string[] = [];
      index += 1;
      while (index < lines.length && !lines[index].startsWith("```")) {
        code.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) index += 1;
      blocks.push({ kind: "code", language, text: code.join("\n") });
      continue;
    }
    if (line.startsWith("> ")) {
      const quote: string[] = [];
      while (index < lines.length && lines[index].startsWith("> ")) {
        quote.push(lines[index].slice(2));
        index += 1;
      }
      blocks.push({ kind: "quote", text: quote.join("\n") });
      continue;
    }
    const listMatch = line.match(/^(?:([-*])|(\d+)\.)\s+(.+)$/);
    if (listMatch !== null) {
      const ordered = listMatch[2] !== undefined;
      const items: string[] = [];
      while (index < lines.length) {
        const item = lines[index].match(/^(?:([-*])|(\d+)\.)\s+(.+)$/);
        if (item === null || (item[2] !== undefined) !== ordered) break;
        items.push(item[3]);
        index += 1;
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }
    if (line.trim() === "") {
      index += 1;
      continue;
    }
    const paragraph = [line];
    index += 1;
    while (
      index < lines.length
      && lines[index].trim() !== ""
      && !lines[index].startsWith("```")
      && !lines[index].startsWith("> ")
      && !/^(#{1,3})\s+/.test(lines[index])
      && !/^ {0,3}(?:-{3,}|\*{3,})\s*$/.test(lines[index])
      && !/^(?:[-*]|\d+\.)\s+/.test(lines[index])
    ) {
      paragraph.push(lines[index]);
      index += 1;
    }
    blocks.push({ kind: "paragraph", text: paragraph.join("\n") });
  }
  return blocks;
}
