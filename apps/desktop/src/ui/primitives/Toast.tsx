import type { CSSProperties, ReactNode } from "react";

import sigilMarkDark from "../../../../../assets/logo/sigil-mark-dark-mode.svg";
import sigilMarkLight from "../../../../../assets/logo/sigil-mark.svg";
import { useLocale } from "../../i18n";
import { Icon } from "../icons";
import { IconButton } from "./IconButton";
import { Tooltip } from "./Tooltip";

export type ToastTone = "info" | "success" | "warning" | "error";

export function Toast({
  children,
  title,
  tone = "info",
  urgent = false,
  timeoutMs,
  onDismiss,
}: {
  readonly children: ReactNode;
  readonly title?: string;
  readonly tone?: ToastTone;
  readonly urgent?: boolean;
  readonly timeoutMs?: number;
  readonly onDismiss?: () => void;
}) {
  const { t } = useLocale();
  const alert = urgent || tone === "error";
  const resolvedTitle = title ?? t(
    tone === "success"
      ? "notificationSuccess"
      : tone === "warning"
        ? "notificationWarning"
        : tone === "error"
          ? "notificationError"
          : "notificationInfo",
  );
  const style = timeoutMs === undefined
    ? undefined
    : { "--sg-toast-duration": `${timeoutMs}ms` } as CSSProperties;
  return (
    <article
      className={`sg-toast sg-toast-${tone}`}
      role={alert ? "alert" : "status"}
      aria-live={alert ? "assertive" : "polite"}
      aria-atomic="true"
      style={style}
    >
      <span className="sg-toast-brand" aria-hidden="true">
        <img className="sg-toast-mark-light" src={sigilMarkLight} alt="" />
        <img className="sg-toast-mark-dark" src={sigilMarkDark} alt="" />
        <span className="sg-toast-signal" />
      </span>
      <span className="sg-toast-copy">
        <strong>{resolvedTitle}</strong>
        <span>{children}</span>
      </span>
      {onDismiss === undefined ? null : (
        <Tooltip label={t("dismissNotification")}>
          <IconButton
            className="sg-toast-dismiss"
            type="button"
            aria-label={t("dismissNotification")}
            icon={<Icon name="close" />}
            onClick={onDismiss}
          />
        </Tooltip>
      )}
      {timeoutMs === undefined ? null : <span className="sg-toast-progress" aria-hidden="true" />}
    </article>
  );
}
