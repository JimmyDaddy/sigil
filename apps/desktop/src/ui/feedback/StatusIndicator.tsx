export type StatusTone = "neutral" | "success" | "warning" | "danger";

export function StatusIndicator({ label, tone = "neutral" }: { readonly label: string; readonly tone?: StatusTone }) {
  return (
    <span className={`sg-status-indicator sg-status-${tone}`}>
      <span aria-hidden="true" />
      <span>{label}</span>
    </span>
  );
}
