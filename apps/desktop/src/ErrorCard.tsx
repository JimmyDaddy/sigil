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
        <button className="quiet-button" type="button" disabled={actionDisabled} onClick={onAction}>{actionLabel}</button>
      ) : null}
    </section>
  );
}
