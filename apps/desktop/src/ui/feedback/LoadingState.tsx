import sigilMarkDark from "../../../../../assets/logo/sigil-mark-dark-mode.svg";
import sigilMarkLight from "../../../../../assets/logo/sigil-mark.svg";

export interface LoadingStateProps {
  readonly label: string;
  readonly detail?: string;
  readonly className?: string;
}

export function LoadingState({ label, detail, className = "" }: LoadingStateProps) {
  return (
    <section
      className={`sg-loading-state ${className}`.trim()}
      role="status"
      aria-label={label}
      aria-live="polite"
      aria-atomic="true"
      aria-busy="true"
    >
      <span className="sg-brand-loader" aria-hidden="true">
        <span className="sg-brand-loader-orbit sg-brand-loader-orbit-outer" />
        <span className="sg-brand-loader-orbit sg-brand-loader-orbit-inner" />
        <span className="sg-brand-loader-mark">
          <img className="sg-brand-loader-mark-light" src={sigilMarkLight} alt="" />
          <img className="sg-brand-loader-mark-dark" src={sigilMarkDark} alt="" />
        </span>
      </span>
      <span className="sg-loading-state-copy">
        <strong>{label}</strong>
        {detail === undefined ? null : <span>{detail}</span>}
      </span>
    </section>
  );
}
