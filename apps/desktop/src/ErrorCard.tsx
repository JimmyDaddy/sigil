import type { ReactNode } from "react";

import { Button, IconButton, Tooltip } from "./ui/primitives";

export function ErrorCard({
  title,
  message,
  actionLabel,
  actionIcon,
  actionDisabled = false,
  onAction,
}: {
  title: string;
  message: string;
  actionLabel?: string;
  actionIcon?: ReactNode;
  actionDisabled?: boolean;
  onAction?: () => void;
}) {
  return (
    <section className="error-card" role="alert">
      <div><strong>{title}</strong><p>{message}</p></div>
      {actionLabel !== undefined && onAction !== undefined ? (
        actionIcon === undefined ? (
          <Button variant="quiet" type="button" disabled={actionDisabled} onClick={onAction}>{actionLabel}</Button>
        ) : (
          <Tooltip label={actionLabel}>
            <IconButton
              className="error-card-icon-action"
              type="button"
              disabled={actionDisabled}
              onClick={onAction}
              aria-label={actionLabel}
              icon={actionIcon}
            />
          </Tooltip>
        )
      ) : null}
    </section>
  );
}
