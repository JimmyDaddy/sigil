import { useState } from "react";

import { useLocale } from "./i18n";
import { MessageContent } from "./MessageContent";

export interface MessageView {
  key: string;
  kind: "user" | "assistant" | "reasoning" | "progress" | "notice" | "error";
  label: string;
  text: string;
  status?: string;
}

export function Message({
  message,
  onOpenExternalUrl,
}: {
  readonly message: MessageView;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
}) {
  const { t } = useLocale();
  const [disclosureOpen, setDisclosureOpen] = useState(false);
  if (message.kind === "reasoning" || message.kind === "progress") {
    return (
      <details
        className={`message-disclosure message-${message.kind}`}
        onToggle={(event) => setDisclosureOpen(event.currentTarget.open)}
      >
        <summary>
          <span className="message-disclosure-title">
            <span>{message.label}</span>
            {message.status ? <small>{message.status}</small> : null}
          </span>
          <small>{t(disclosureOpen ? "hideDetails" : "showDetails")}</small>
        </summary>
        <MessageContent text={message.text} onOpenExternalUrl={onOpenExternalUrl} />
      </details>
    );
  }
  return (
    <article className={`message message-${message.kind}${message.status ? ` message-status-${message.status}` : ""}`}>
      <header><span>{message.label}</span>{message.status ? <small>{message.status}</small> : null}</header>
      <MessageContent text={message.text} onOpenExternalUrl={onOpenExternalUrl} />
    </article>
  );
}
