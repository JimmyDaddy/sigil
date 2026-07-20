import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";

import { ApprovalDock } from "../../ApprovalDock";
import { Composer } from "../../Composer";
import { DiffViewer } from "../../DiffViewer";
import { HistoryContent } from "../../HistoryPanel";
import { Message } from "../../Message";
import { ToolCard } from "../../ToolCard";
import { VerificationInspector } from "../../VerificationInspector";
import { PrimitiveCatalog } from "./PrimitiveCatalog";
import {
  catalogFixtures,
  type CatalogContrast,
  type CatalogFixture,
  type CatalogMotion,
  type CatalogTheme,
  type CatalogViewport,
} from "./fixtures";
import { UiCatalog } from "./UiCatalog";

export function CatalogApp() {
  const [fixtureId, setFixtureId] = useState(catalogFixtures[0]?.id ?? "no-workspace");
  const [theme, setTheme] = useState<CatalogTheme>("system");
  const [viewport, setViewport] = useState<CatalogViewport>(1280);
  const [contrast, setContrast] = useState<CatalogContrast>("normal");
  const [motion, setMotion] = useState<CatalogMotion>("full");
  const [zoom, setZoom] = useState<1 | 2>(1);
  const fixture = useMemo(
    () => catalogFixtures.find((candidate) => candidate.id === fixtureId) ?? catalogFixtures[0],
    [fixtureId],
  );

  useEffect(() => {
    const root = document.documentElement;
    const resolved = theme === "system"
      ? (window.matchMedia?.("(prefers-color-scheme: light)").matches ? "light" : "dark")
      : theme;
    root.dataset.theme = resolved;
    root.dataset.themePreference = theme;
    root.dataset.catalogContrast = contrast;
    root.dataset.catalogMotion = motion;
    root.style.colorScheme = resolved;
  }, [contrast, motion, theme]);

  if (fixture === undefined) return null;
  return (
    <main className="catalog-shell">
      <header className="catalog-toolbar">
        <div>
          <p className="eyebrow">Development only</p>
          <h1>Sigil UI catalog</h1>
          <p>Real internal components, synthetic data, and no desktop bridge or workspace access.</p>
        </div>
        <CatalogSelect label="Fixture" value={fixture.id} onChange={setFixtureId} options={catalogFixtures.map(({ id }) => id)} />
        <CatalogSelect label="Theme" value={theme} onChange={(value) => setTheme(value as CatalogTheme)} options={fixture.themes} />
        <CatalogSelect label="Viewport" value={String(viewport)} onChange={(value) => setViewport(Number(value) as CatalogViewport)} options={fixture.viewports.map(String)} />
        <CatalogSelect label="Contrast" value={contrast} onChange={(value) => setContrast(value as CatalogContrast)} options={fixture.contrastModes} />
        <CatalogSelect label="Motion" value={motion} onChange={(value) => setMotion(value as CatalogMotion)} options={fixture.motionModes} />
        <CatalogSelect label="Zoom" value={String(zoom)} onChange={(value) => setZoom(Number(value) as 1 | 2)} options={fixture.zoomFactors.map(String)} />
      </header>
      <p className="catalog-evidence" role="status">
        {viewport}px viewport · {zoom === 1 ? "100%" : "200%"} zoom · {theme} theme · {contrast} · {motion} motion
      </p>
      <div className="catalog-stage">
        <div
          className="catalog-frame"
          style={{
            "--catalog-frame-width": `${viewport / zoom}px`,
            "--catalog-frame-zoom": String(zoom),
          } as CSSProperties}
        >
          <UiCatalog fixture={fixture}>
            <FixtureSurface fixture={fixture} />
          </UiCatalog>
        </div>
      </div>
    </main>
  );
}

function CatalogSelect({
  label,
  value,
  options,
  onChange,
}: {
  readonly label: string;
  readonly value: string;
  readonly options: readonly string[];
  readonly onChange: (value: string) => void;
}) {
  return (
    <label className="catalog-control">
      <span>{label}</span>
      <select value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map((option) => <option key={option}>{option}</option>)}
      </select>
    </label>
  );
}

function FixtureSurface({ fixture }: { readonly fixture: CatalogFixture }) {
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const [approvalMode, setApprovalMode] = useState<"ask" | "allow_readonly" | "deny">("ask");
  const counts = fixture.degradedCounts;
  return (
    <div className="catalog-fixture-surface">
      {fixture.id === "no-workspace" ? (
        <div className="catalog-empty"><strong>No workspace is open.</strong><span>Open a project to browse its conversations.</span></div>
      ) : null}
      {fixture.sessions !== undefined ? (
        <section className="catalog-session-surface" aria-label="Conversation navigation fixture">
          <HistoryContent
            state="ready"
            page={{
              workspaceId: "catalog-workspace",
              generation: 1,
              reconciledAtUnixMs: 1_784_419_200_000,
              degradedSourceCount: counts?.unavailable ?? 0,
              identityConflictCount: counts?.changed ?? 0,
              truncatedSourceCount: counts?.truncated ?? 0,
              entries: [...fixture.sessions],
            }}
            onRetry={() => undefined}
            onLoadMore={() => undefined}
            onOpen={() => undefined}
          />
        </section>
      ) : null}
      {fixture.streamState !== undefined ? (
        <div className={`stream-chip stream-${fixture.streamState}`}>{fixture.streamState}</div>
      ) : null}
      {fixture.attachmentGap ? <div className="timeline-gap">Some live details were not retained while reconnecting.</div> : null}
      {fixture.composer === undefined ? null : (
        <div className="catalog-composer-surface">
          <Composer
            draftKey="sigil:catalog-composer"
            active={fixture.composer.active}
            submitting={false}
            controlBusy={false}
            composerRef={composerRef}
            runContext={fixture.composer.context}
            runContextBusy={false}
            approvalMode={approvalMode}
            onApprovalModeChange={setApprovalMode}
            onSubmit={async () => false}
            onCancel={() => undefined}
          />
        </div>
      )}
      {fixture.tool === undefined ? null : <ToolCard tool={fixture.tool} />}
      {fixture.approval === undefined ? null : (
        <>
          <textarea className="sr-only" ref={composerRef} aria-label="Catalog composer" />
          <ApprovalDock approval={fixture.approval} busy={false} composerRef={composerRef} onDecision={() => undefined} />
        </>
      )}
      {fixture.verification === undefined ? null : (
        <VerificationInspector verification={fixture.verification} busy={false} runActive={false} onRerun={() => undefined} />
      )}
      {fixture.diff === undefined ? null : <DiffViewer diff={fixture.diff} />}
      {fixture.longCopy === undefined ? null : (
        <Message message={{ key: "catalog-long-copy", kind: "assistant", label: "Sigil", text: fixture.longCopy }} />
      )}
      {fixture.id === "empty-catalog" || fixture.id === "no-workspace" ? <PrimitiveCatalog /> : null}
    </div>
  );
}
