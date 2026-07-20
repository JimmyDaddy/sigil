import { Button } from "./ui/primitives";

export function ErrorCard({
  title,
  message,
  actionLabel,
  actionDisabled = false,
  onAction,
}: {
  title: string;
  message: string;
  actionLabel?: string;
  actionDisabled?: boolean;
  onAction?: () => void;
}) {
  return (
    <section className="error-card" role="alert">
      <div><strong>{title}</strong><p>{message}</p></div>
      {actionLabel !== undefined && onAction !== undefined ? (
        <Button variant="quiet" type="button" disabled={actionDisabled} onClick={onAction}>{actionLabel}</Button>
      ) : null}
    </section>
  );
}
