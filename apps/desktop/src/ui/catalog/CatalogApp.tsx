import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";

import { ApprovalDock } from "../../ApprovalDock";
import { Composer } from "../../Composer";
import { ConversationQueuePanel } from "../../ConversationQueuePanel";
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
import { Select } from "../primitives";
import {
  resolveSystemTheme,
  themeColorScheme,
} from "../../appearance/resolveTheme";
import type { ResolvedTheme } from "../../appearance/contract";
import type { ConversationQueueView } from "../../types";

const catalogQueue: ConversationQueueView = {
  schemaVersion: 1,
  sessionId: "catalog-session",
  generation: "catalog-queue-v1",
  paused: false,
  totalItems: 2,
  items: [
    {
      entryId: "catalog-queue-first",
      order: 0,
      kind: "chat",
      status: "queued",
      promptPreview: "Run the focused parser tests next",
      promptPreviewTruncated: false,
      promptMaterial: "persisted_safe",
      dispatchable: false,
      blockedReason: "foreground_run_active",
    },
    {
      entryId: "catalog-queue-second",
      order: 1,
      kind: "chat",
      status: "queued",
      promptPreview: "Then summarize any remaining failures",
      promptPreviewTruncated: false,
      promptMaterial: "persisted_safe",
      dispatchable: false,
      blockedReason: "foreground_run_active",
    },
  ],
  truncated: false,
};

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
    const resolved: ResolvedTheme = theme === "system"
      ? resolveSystemTheme()
      : theme;
    const colorScheme = themeColorScheme(resolved);
    root.dataset.theme = resolved;
    root.dataset.themePreference = theme;
    root.dataset.colorScheme = colorScheme;
    root.dataset.catalogContrast = contrast;
    root.dataset.catalogMotion = motion;
    root.style.colorScheme = colorScheme;
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
            <FixtureSurface fixture={fixture} theme={theme} />
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
    <Select
      label={label}
      containerClassName="catalog-control"
      value={value}
      onChange={(event) => onChange(event.target.value)}
    >
        {options.map((option) => <option key={option}>{option}</option>)}
    </Select>
  );
}

function FixtureSurface({
  fixture,
  theme,
}: {
  readonly fixture: CatalogFixture;
  readonly theme: CatalogTheme;
}) {
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const [permissionMode, setPermissionMode] = useState<"read-only" | "manual" | "auto-edit" | "danger-full-access">("manual");
  const [reasoningEffort, setReasoningEffort] = useState<"low" | "medium" | "high" | "max">("max");
  const counts = fixture.degradedCounts;
  if (fixture.fullWorkbench) {
    return (
      <iframe
        className="catalog-workbench-frame"
        title="Complete Sigil workbench fixture"
        src={`/catalog-workbench.html?theme=${theme}`}
      />
    );
  }
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
            onRename={() => undefined}
            onDelete={() => undefined}
            onDeleteInvalidSource={() => undefined}
            onQuarantine={() => undefined}
          />
        </section>
      ) : null}
      {fixture.streamState !== undefined ? (
        <div className={`conversation-activity stream-${fixture.streamState}`}><span className="conversation-activity-dot" />{fixture.streamState}</div>
      ) : null}
      {fixture.attachmentGap ? <div className="timeline-gap">Some live details were not retained while reconnecting.</div> : null}
      {fixture.composer === undefined ? null : (
        <div className="catalog-composer-surface">
          <Composer
            draftKey="sigil:catalog-composer"
            active={fixture.composer.active}
            submissionBlocked={false}
            submitting={false}
            controlBusy={false}
            composerRef={composerRef}
            runContext={fixture.composer.context}
            runContextBusy={false}
            selectedModelName={fixture.composer.context.modelName}
            permissionMode={permissionMode}
            reasoningEffort={reasoningEffort}
            requestedSkill={undefined}
            queueCount={catalogQueue.totalItems}
            queuePaused={catalogQueue.paused}
            queueBusy={false}
            queuePanel={(
              <ConversationQueuePanel
                queue={catalogQueue}
                busy={false}
                error={false}
                reasoningEffort={reasoningEffort}
                onRefresh={() => undefined}
                onCommand={async () => false}
              />
            )}
            onModelChange={() => undefined}
            onNewSession={() => Promise.resolve(true)}
            onOpenSessionPicker={() => undefined}
            onOpenSettings={() => undefined}
            onOpenSupport={() => undefined}
            onOpenAgentWorkbench={() => undefined}
            onOpenQueue={() => undefined}
            onPreviewCompaction={() => undefined}
            onNotice={() => undefined}
            onPermissionModeChange={setPermissionMode}
            onReasoningEffortChange={setReasoningEffort}
            onSubmit={async () => false}
            onInterruptAndRunNext={async () => false}
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
