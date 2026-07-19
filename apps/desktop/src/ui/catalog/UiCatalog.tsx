import type { ReactNode } from "react";

import { UI_CATALOG_MARKER, type CatalogFixture } from "./fixtures";

export interface UiCatalogProps {
  readonly fixture: CatalogFixture;
  readonly children: ReactNode;
}

/** Development-only fixture host. Production entrypoints must never import this module. */
export function UiCatalog({ fixture, children }: UiCatalogProps) {
  return (
    <section data-ui-catalog={UI_CATALOG_MARKER} data-fixture={fixture.id}>
      <header>
        <strong>{fixture.id}</strong>
        <span>{fixture.description}</span>
      </header>
      {children}
    </section>
  );
}
