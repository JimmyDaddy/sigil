import sigilMarkDark from "../../../../../assets/logo/sigil-mark-dark-mode.svg";
import sigilMarkLight from "../../../../../assets/logo/sigil-mark.svg";

import { Icon } from "../icons";
import { Button } from "../primitives";

export function PaginationControl({
  label,
  loadingLabel,
  loading,
  onLoadMore,
  className = "",
}: {
  readonly label: string;
  readonly loadingLabel: string;
  readonly loading: boolean;
  readonly onLoadMore: () => void;
  readonly className?: string;
}) {
  return (
    <div className={`sg-pagination-control ${className}`.trim()}>
      <Button
        className="sg-pagination-button"
        type="button"
        busy={loading}
        leadingIcon={loading ? <PaginationLoader /> : <Icon name="chevron-down" />}
        onClick={onLoadMore}
      >
        {loading ? loadingLabel : label}
      </Button>
    </div>
  );
}

function PaginationLoader() {
  return (
    <span className="sg-pagination-loader" aria-hidden="true">
      <span className="sg-pagination-loader-ring" />
      <img className="sg-pagination-loader-mark sg-pagination-loader-mark-light" src={sigilMarkLight} alt="" />
      <img className="sg-pagination-loader-mark sg-pagination-loader-mark-dark" src={sigilMarkDark} alt="" />
    </span>
  );
}
