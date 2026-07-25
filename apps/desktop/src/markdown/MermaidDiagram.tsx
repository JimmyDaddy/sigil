import { useEffect, useId, useMemo, useRef, useState } from "react";

import { writeClipboard } from "../clipboard";
import { useLocale } from "../i18n";
import { Icon } from "../ui/icons";
import { Button, IconButton, Tooltip } from "../ui/primitives";
import { admitMermaid, sanitizeMermaidSvg } from "./mermaidSecurity";
import {
  diagramCacheKey,
  readDiagramCache,
  writeDiagramCache,
  type DiagramCacheEntry,
} from "./renderCache";

type DiagramState =
  | { readonly kind: "source_preview" }
  | { readonly kind: "loading" }
  | ({ readonly kind: "ready" } & DiagramCacheEntry)
  | { readonly kind: "error"; readonly summary: string };

let renderQueue = Promise.resolve();

export function MermaidDiagram({
  source,
  contentId,
}: {
  readonly source: string;
  readonly contentId: string;
}) {
  const { t } = useLocale();
  const reactId = useId().replace(/[^A-Za-z0-9_-]/gu, "");
  const theme = document.documentElement.dataset.theme ?? "sigil_dark";
  const admission = useMemo(() => admitMermaid(source), [source]);
  const renderId = `sigil-mermaid-${safeId(contentId)}-${reactId}`;
  const cacheKey = diagramCacheKey(source, theme);
  const [state, setState] = useState<DiagramState>(() => {
    const cached = readDiagramCache(cacheKey);
    return cached === undefined ? { kind: "source_preview" } : { kind: "ready", ...cached };
  });
  const [showSource, setShowSource] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [scale, setScale] = useState(1);
  const generation = useRef(0);

  useEffect(() => {
    generation.current += 1;
    const currentGeneration = generation.current;
    if (!admission.accepted) {
      setState({ kind: "error", summary: t("diagramRejected") });
      return;
    }
    const cached = readDiagramCache(cacheKey);
    if (cached !== undefined) {
      setState({ kind: "ready", ...cached });
      return;
    }

    setState({ kind: "loading" });
    const task = async () => {
      try {
        const module = await import("mermaid");
        const mermaid = module.default;
        const computed = getComputedStyle(document.documentElement);
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          suppressErrorRendering: true,
          maxTextSize: 32 * 1024,
          theme: "base",
          themeVariables: {
            background: cssToken(computed, "--sg-sys-color-surface"),
            primaryColor: cssToken(computed, "--sg-sys-color-primary-container"),
            primaryTextColor: cssToken(computed, "--sg-sys-color-on-surface"),
            primaryBorderColor: cssToken(computed, "--sg-sys-color-outline"),
            lineColor: cssToken(computed, "--sg-sys-color-on-surface-variant"),
            fontFamily: "system-ui, sans-serif",
          },
          flowchart: { htmlLabels: false },
        });
        const result = await mermaid.render(renderId, source);
        const svg = sanitizeMermaidSvg(
          result.svg,
          renderId,
          `${t("diagram")}: ${admission.diagramType}`,
        );
        if (svg === undefined) throw new Error("unsafe diagram output");
        const entry = { svg, diagramType: admission.diagramType };
        writeDiagramCache(cacheKey, entry);
        if (generation.current === currentGeneration) setState({ kind: "ready", ...entry });
      } catch {
        if (generation.current === currentGeneration) {
          setState({ kind: "error", summary: t("diagramRenderFailed") });
        }
      }
    };
    renderQueue = renderQueue.then(task, task);
    return () => {
      generation.current += 1;
    };
  }, [admission, cacheKey, renderId, source, t, theme]);

  const diagramType = state.kind === "ready" ? state.diagramType : admission.diagramType;
  const copySource = () => void writeClipboard(source);
  return (
    <section className={`mermaid-card is-${state.kind}${expanded ? " is-expanded" : ""}`}>
      <header>
        <span>
          <strong>{t("diagram")}</strong>
          <small>{diagramType} · {diagramStateLabel(state.kind, t)}</small>
        </span>
        <span className="mermaid-actions">
          <Tooltip label={t("copyDiagramSource")}>
            <IconButton
              type="button"
              aria-label={t("copyDiagramSource")}
              icon={<Icon name="copy" />}
              onClick={copySource}
            />
          </Tooltip>
          <Button type="button" variant="quiet" onClick={() => setShowSource((current) => !current)}>
            {t(showSource ? "hideDiagramSource" : "showDiagramSource")}
          </Button>
          <Button type="button" variant="quiet" onClick={() => setExpanded((current) => !current)}>
            {t(expanded ? "closeDiagramViewer" : "openDiagramViewer")}
          </Button>
        </span>
      </header>
      {state.kind === "loading" ? (
        <div className="mermaid-loading" role="status" aria-live="polite">
          <span className="mermaid-loading-mark" aria-hidden="true">◇</span>
          <span>{t("renderingDiagram")}</span>
        </div>
      ) : null}
      {state.kind === "ready" ? (
        <div className="mermaid-viewport">
          <div
            className="mermaid-svg"
            style={{ transform: `scale(${scale})` }}
            // The SVG is generated locally, strict-rendered, and sanitized in mermaidSecurity.ts.
            dangerouslySetInnerHTML={{ __html: state.svg }}
          />
        </div>
      ) : null}
      {state.kind === "error" ? <p className="mermaid-error">{state.summary}</p> : null}
      {expanded && state.kind === "ready" ? (
        <div className="mermaid-viewer-controls">
          <Button type="button" variant="quiet" onClick={() => setScale((value) => Math.max(.5, value - .1))}>−</Button>
          <span>{Math.round(scale * 100)}%</span>
          <Button type="button" variant="quiet" onClick={() => setScale((value) => Math.min(3, value + .1))}>+</Button>
          <Button type="button" variant="quiet" onClick={() => setScale(1)}>{t("fitDiagram")}</Button>
        </div>
      ) : null}
      {showSource || state.kind === "error" || state.kind === "source_preview" ? (
        <pre className="mermaid-source"><code>{source}</code></pre>
      ) : null}
    </section>
  );
}

function safeId(value: string): string {
  return value.replace(/[^A-Za-z0-9_-]/gu, "-").slice(0, 64) || "message";
}

function cssToken(styles: CSSStyleDeclaration, name: string): string {
  return styles.getPropertyValue(name).trim() || "#888888";
}

function diagramStateLabel(
  state: DiagramState["kind"],
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (state) {
    case "ready": return t("diagramReady");
    case "loading": return t("diagramLoading");
    case "error": return t("diagramError");
    case "source_preview": return t("diagramSource");
  }
}
