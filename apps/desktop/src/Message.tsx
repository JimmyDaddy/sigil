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
  if (message.kind === "reasoning" || message.kind === "progress") {
    return (
      <details className={`message-disclosure message-${message.kind}`}>
        <summary><span>{message.label}</span><small>{message.status ?? "Show details"}</small></summary>
        <MessageContent text={message.text} onOpenExternalUrl={onOpenExternalUrl} />
      </details>
    );
  }
  return (
    <article className={`message message-${message.kind}`}>
      <header><span>{message.label}</span>{message.status ? <small>{message.status}</small> : null}</header>
      <MessageContent text={message.text} onOpenExternalUrl={onOpenExternalUrl} />
    </article>
  );
}
