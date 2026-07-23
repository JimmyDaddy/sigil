import { useLayoutEffect, useState } from "react";

import { useLocale } from "./i18n";
import { MessageContent } from "./MessageContent";

const DISCLOSURE_PREVIEW_LINES = 3;
const DISCLOSURE_PREVIEW_CHARACTERS = 360;

export interface MessageView {
  key: string;
  kind: "user" | "assistant" | "reasoning" | "progress" | "notice" | "error";
  label: string;
  text: string;
  status?: string;
}

export function Message({
  message,
  displayId,
  onOpenExternalUrl,
}: {
  readonly message: MessageView;
  readonly displayId?: string;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
}) {
  const { t } = useLocale();
  const streaming = message.status === "streaming";
  const [disclosureOpen, setDisclosureOpen] = useState(streaming);
  useLayoutEffect(() => {
    setDisclosureOpen(streaming);
  }, [message.key, streaming]);
  if (message.kind === "reasoning" || message.kind === "progress") {
    const preview = disclosurePreview(message.text);
    return (
      <details
        className={`message-disclosure message-${message.kind}`}
        data-display-id={displayId}
        open={disclosureOpen}
        onToggle={(event) => setDisclosureOpen(event.currentTarget.open)}
      >
        <summary>
          <span className="message-disclosure-title">
            <span>{message.label}</span>
            {message.status ? <small>{message.status}</small> : null}
          </span>
          <small>{t(disclosureOpen ? "hideDetails" : "showDetails")}</small>
        </summary>
        {!disclosureOpen && preview !== "" ? (
          <p className="message-disclosure-preview">{preview}</p>
        ) : null}
        <MessageContent text={message.text} onOpenExternalUrl={onOpenExternalUrl} />
      </details>
    );
  }
  return (
    <article
      className={`message message-${message.kind}${message.status ? ` message-status-${message.status}` : ""}`}
      data-display-id={displayId}
    >
      <header><span>{message.label}</span>{message.status ? <small>{message.status}</small> : null}</header>
      <MessageContent text={message.text} onOpenExternalUrl={onOpenExternalUrl} />
    </article>
  );
}

function disclosurePreview(text: string): string {
  const trimmed = text.trim();
  if (trimmed === "") return "";
  const linePreview = trimmed.split("\n").slice(0, DISCLOSURE_PREVIEW_LINES).join("\n");
  if (linePreview.length <= DISCLOSURE_PREVIEW_CHARACTERS) return linePreview;
  return `${linePreview.slice(0, DISCLOSURE_PREVIEW_CHARACTERS - 1).trimEnd()}…`;
}
