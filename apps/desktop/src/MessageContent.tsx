import { useState } from "react";

import { writeClipboard } from "./clipboard";
import { useLocale } from "./i18n";
import { SafeMarkdown } from "./SafeMarkdown";
import { Icon } from "./ui/icons";
import { IconButton, Tooltip } from "./ui/primitives";

interface MessageContentProps {
  readonly text: string;
  readonly onOpenExternalUrl?: (url: string) => Promise<void>;
}

export function MessageContent({ text, onOpenExternalUrl }: MessageContentProps) {
  const { t } = useLocale();
  const [copied, setCopied] = useState(false);
  if (text.trim() === "") {
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
      <SafeMarkdown text={text} onOpenExternalUrl={onOpenExternalUrl} />
    </div>
  );
}
