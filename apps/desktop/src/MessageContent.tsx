import { Fragment, useState } from "react";

import { writeClipboard } from "./clipboard";
import { Button } from "./ui/primitives";

interface MessageContentProps {
  text: string;
}

type ContentBlock =
  | { kind: "code"; language?: string; text: string }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "quote"; text: string }
  | { kind: "paragraph"; text: string };

export function MessageContent({ text }: MessageContentProps) {
  const [copied, setCopied] = useState(false);
  const blocks = parseBlocks(text);
  return (
    <div className="message-content">
      <Button
        className="content-copy"
        variant="quiet"
        type="button"
        onClick={() => void writeClipboard(text).then(setCopied)}
        aria-label="Copy message"
      >
        {copied ? "Copied" : "Copy"}
      </Button>
      {blocks.map((block, index) => {
        const key = `${block.kind}:${index}`;
        if (block.kind === "code") {
          return (
            <div className="code-block" key={key}>
              <header><span>{block.language ?? "code"}</span><CopyButton text={block.text} label="Copy code" /></header>
              <pre><code>{block.text}</code></pre>
            </div>
          );
        }
        if (block.kind === "quote") return <blockquote key={key}>{inlineCode(block.text)}</blockquote>;
        if (block.kind === "list") {
          const List = block.ordered ? "ol" : "ul";
          return <List key={key}>{block.items.map((item, itemIndex) => <li key={`${itemIndex}:${item}`}>{inlineCode(item)}</li>)}</List>;
        }
        return <p key={key}>{inlineCode(block.text)}</p>;
      })}
    </div>
  );
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button className="inline-copy" variant="quiet" type="button" onClick={() => void writeClipboard(text).then(setCopied)} aria-label={label}>
      {copied ? "Copied" : "Copy"}
    </Button>
  );
}

function inlineCode(text: string) {
  return text.split(/(`[^`]+`)/g).map((part, index) =>
    part.startsWith("`") && part.endsWith("`")
      ? <code key={`${index}:${part}`}>{part.slice(1, -1)}</code>
      : <Fragment key={`${index}:${part}`}>{part}</Fragment>,
  );
}

export function parseBlocks(text: string): ContentBlock[] {
  const lines = text.replaceAll("\r\n", "\n").split("\n");
  const blocks: ContentBlock[] = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
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
      && !/^(?:[-*]|\d+\.)\s+/.test(lines[index])
    ) {
      paragraph.push(lines[index]);
      index += 1;
    }
    blocks.push({ kind: "paragraph", text: paragraph.join("\n") });
  }
  return blocks.length === 0 ? [{ kind: "paragraph", text: "No text payload." }] : blocks;
}
