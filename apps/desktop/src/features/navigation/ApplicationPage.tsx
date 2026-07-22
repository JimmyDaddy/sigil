import { useId, type ReactNode } from "react";

import { Icon } from "../../ui/icons";
import { IconButton, Tooltip } from "../../ui/primitives";

export interface ApplicationPageNavigation {
  readonly label: string;
  readonly onBack: () => void;
}

export function ApplicationPage({
  eyebrow,
  title,
  detail,
  navigation,
  aside,
  className = "",
  children,
}: {
  readonly eyebrow: string;
  readonly title: string;
  readonly detail: string;
  readonly navigation?: ApplicationPageNavigation;
  readonly aside?: ReactNode;
  readonly className?: string;
  readonly children: ReactNode;
}) {
  const titleId = useId();

  return (
    <section
      className={`application-page${navigation === undefined ? "" : " application-page-has-toolbar"} ${className}`.trim()}
      aria-labelledby={titleId}
    >
      {navigation === undefined ? null : (
        <header className="application-page-toolbar">
          <div className="application-page-toolbar-leading">
            <Tooltip label={navigation.label}>
              <IconButton
                className="application-page-back"
                type="button"
                aria-label={navigation.label}
                icon={<Icon name="back" />}
                onClick={navigation.onBack}
              />
            </Tooltip>
            <h1 id={titleId}>{title}</h1>
          </div>
          {aside === undefined ? null : <div className="application-page-toolbar-actions">{aside}</div>}
        </header>
      )}

      <div className="application-page-content">
        {navigation === undefined ? (
          <header className="application-page-header">
            <div className="application-page-heading">
              <p className="eyebrow">{eyebrow}</p>
              <h1 id={titleId}>{title}</h1>
              <p>{detail}</p>
            </div>
            {aside}
          </header>
        ) : (
          <div className="application-page-intro">
            <p className="eyebrow">{eyebrow}</p>
            <p>{detail}</p>
          </div>
        )}
        {children}
      </div>
    </section>
  );
}
