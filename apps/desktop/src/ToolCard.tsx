import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";

import { DiffViewer, isUnifiedDiff } from "./DiffViewer";
import { HighlightedCode } from "./SafeMarkdown";
import { translateEnglish, useLocale, type Translate } from "./i18n";
import { presentArtifactStatus } from "./statusPresentation";
import type {
  ToolArtifactAvailability,
  ToolArtifactPage,
  ToolArtifactSelector,
  UserInputReceipt,
} from "./types";
import { Icon, type IconName } from "./ui/icons";
import { Button, IconButton, TextField, Tooltip } from "./ui/primitives";

const MAX_VISIBLE_OUTPUT_LINES = 240;
const OUTPUT_PREVIEW_LINES = 3;
const OUTPUT_PREVIEW_CHARACTERS = 480;
const MAX_SUMMARY_CHARACTERS = 280;
const SHORT_OUTPUT_BYTES = 4_096;
const ARTIFACT_CACHE_MAX_PAGES = 8;
const ARTIFACT_CACHE_MAX_BYTES = 64 * 1_024;

export interface ToolView {
  key: string;
  toolName: string;
  text: string;
  input?: string;
  status?: string;
  risk?: string;
  duration?: string;
  executionStartedAtMs?: number;
  executionUpdatedAtMs?: number;
  artifactRef?: string;
  artifactAvailability?: ToolArtifactAvailability;
  artifactHasMore?: boolean;
  artifactPersistedBytes?: number;
  userInputReceipt?: UserInputReceipt;
}


function useCommandDuration(tool: ToolView): string | undefined {
  const [now, setNow] = useState(Date.now);
  const running = tool.status === "running" || tool.status === "starting";
  const started = tool.executionStartedAtMs;
  useEffect(() => {
    if (!running || started === undefined) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [running, started]);
  if (started === undefined) return tool.duration;
  if (["pending", "requested", "approval", "awaiting_approval", "waiting_for_approval", "denied"].includes(tool.status ?? "")) return undefined;
  const until = running ? now : tool.executionUpdatedAtMs;
  return until === undefined ? tool.duration : `${(Math.max(0, until - started) / 1_000).toFixed(1)} s`;
}

type ToolTone = "neutral" | "info" | "success" | "warning" | "danger";

interface ToolPresentation {
  readonly displayName: string;
  readonly status: string;
  readonly tone: ToolTone;
  readonly summary?: string;
  readonly input?: string;
  readonly detailLabel?: string;
  readonly detailKind?: "diff" | "output" | "raw";
  readonly detailText?: string;
  readonly detailLanguage?: string;
  readonly streaming: boolean;
}

interface StructuredOutput {
  readonly status?: string;
  readonly summary?: string;
  readonly input?: string;
  readonly output?: string;
  readonly language?: string;
  readonly hasError: boolean;
}

interface OutputPresentation {
  readonly summary: string;
  readonly detailText?: string;
  readonly language?: string;
}

interface OutputPreview {
  readonly text: string;
  readonly truncated: boolean;
}

interface ArtifactReadingPosition {
  readonly readerOpen: boolean;
  readonly page?: ToolArtifactPage;
  readonly previousSelectors: readonly ToolArtifactSelector[];
  readonly scrollTop: number;
}

interface CachedArtifactPage {
  readonly page: ToolArtifactPage;
  readonly bodyBytes: number;
}

export function ToolCard({
  tool,
  displayId,
  onReadArtifact,
  onCancelExecution,
  cancelling = false,
}: {
  readonly tool: ToolView;
  readonly displayId?: string;
  readonly onReadArtifact?: (selector: ToolArtifactSelector) => Promise<ToolArtifactPage>;
  readonly onCancelExecution?: () => void;
  readonly cancelling?: boolean;
}) {
  const { t } = useLocale();
  const presentation = presentTool(tool, t);
  const compactCompletedRead = tool.toolName === "read_file" && presentation.tone === "success"
    && tool.artifactPersistedBytes !== undefined && tool.artifactPersistedBytes <= SHORT_OUTPUT_BYTES;
  const questionReceipt = tool.toolName === "request_user_input"
    && tool.userInputReceipt !== undefined && statusTone(tool.status) === "success";
  const duration = useCommandDuration(tool);
  const boundedDetail = boundedOutput(presentation.detailText ?? "");
  const inputLanguage = toolInputLanguage(tool.toolName);
  const [artifactPage, setArtifactPage] = useState<ToolArtifactPage>();
  const [artifactPreviousSelectors, setArtifactPreviousSelectors] = useState<readonly ToolArtifactSelector[]>([]);
  const artifactReadingPosition = useRef<ArtifactReadingPosition | undefined>(undefined);
  const artifactPageCache = useRef(new Map<string, CachedArtifactPage>());
  const artifactCurrentPosition = useRef<ArtifactReadingPosition>({ readerOpen: false, previousSelectors: [], scrollTop: 0 });
  const artifactContent = useRef<HTMLDivElement>(null);
  const artifactScrollTop = useRef(0);
  const artifactRestoreFocus = useRef(false);
  const [artifactBusy, setArtifactBusy] = useState(false);
  const [artifactError, setArtifactError] = useState(false);
  const [artifactQuery, setArtifactQuery] = useState("");
  const [artifactSearchOpen, setArtifactSearchOpen] = useState(false);
  const [artifactReaderOpen, setArtifactReaderOpen] = useState(false);
  const artifactReaderId = useId();
  const artifactReaderToggle = useRef<HTMLButtonElement>(null);
  artifactCurrentPosition.current = { readerOpen: artifactReaderOpen, page: artifactPage,
    previousSelectors: artifactPreviousSelectors, scrollTop: artifactContent.current?.scrollTop ?? artifactScrollTop.current };
  const artifactSearchId = useId();
  const artifactSearchInput = useRef<HTMLInputElement>(null);
  const artifactSearchToggle = useRef<HTMLButtonElement>(null);
  const canReadArtifact =
    tool.artifactRef !== undefined
    && tool.artifactAvailability === "available"
    && onReadArtifact !== undefined;
  const artifactReadGeneration = useRef(0);
  const artifactReadBusy = useRef(false);
  const readArtifactRef = useRef(onReadArtifact);
  readArtifactRef.current = onReadArtifact;
  useLayoutEffect(() => {
    artifactReadGeneration.current += 1;
    artifactReadBusy.current = false;
    setArtifactPage(undefined);
    setArtifactPreviousSelectors([]);
    artifactReadingPosition.current = undefined;
    artifactPageCache.current.clear();
    artifactScrollTop.current = 0;
    artifactRestoreFocus.current = false;
    setArtifactBusy(false);
    setArtifactError(false);
    setArtifactQuery("");
    setArtifactSearchOpen(false);
    setArtifactReaderOpen(false);
    return () => {
      artifactReadGeneration.current += 1;
      artifactReadBusy.current = false;
    };
  }, [tool.key, tool.artifactRef, tool.artifactAvailability, canReadArtifact]);
  useEffect(() => {
    if (artifactSearchOpen) artifactSearchInput.current?.focus();
  }, [artifactSearchOpen]);
  useLayoutEffect(() => {
    if (artifactContent.current === null) return;
    if (artifactRestoreFocus.current) artifactContent.current.focus({ preventScroll: true });
    artifactContent.current.scrollTop = artifactScrollTop.current;
    artifactRestoreFocus.current = false;
  }, [artifactPage, artifactReaderOpen]);
  const readArtifact = useCallback((selector: ToolArtifactSelector, navigation: "replace" | "next" | "previous" = "replace") => {
    const read = readArtifactRef.current;
    if (!canReadArtifact || read === undefined || artifactReadBusy.current) return;
    setArtifactReaderOpen(true);
    const generation = ++artifactReadGeneration.current;
    artifactReadBusy.current = true;
    setArtifactBusy(true);
    setArtifactError(false);
    const previous = { ...artifactCurrentPosition.current, scrollTop: artifactContent.current?.scrollTop ?? artifactScrollTop.current };
    const cached = navigation === "replace" ? undefined : artifactPageCache.current.get(artifactSelectorKey(selector));
    void (async () => {
      try {
        const page = cached?.page ?? await read(selector);
        if (generation === artifactReadGeneration.current) {
          if (selector.kind === "search_literal" && previous.page?.selector.kind !== "search_literal") {
            artifactReadingPosition.current = previous;
          } else if (selector.kind !== "search_literal" && navigation === "replace") {
            artifactReadingPosition.current = undefined;
          }
          cacheArtifactPage(artifactPageCache.current, page, artifactReadingPosition.current?.page);
          artifactScrollTop.current = 0;
          setArtifactPage(page);
          setArtifactPreviousSelectors(navigation === "next" && previous.page !== undefined
            ? [...previous.previousSelectors, previous.page.selector]
            : navigation === "previous" ? previous.previousSelectors.slice(0, -1) : []);
        }
      } catch {
        if (generation === artifactReadGeneration.current) setArtifactError(true);
      } finally {
        if (generation === artifactReadGeneration.current) {
          artifactReadBusy.current = false;
          setArtifactBusy(false);
        }
      }
    })();
    return () => {
      if (generation !== artifactReadGeneration.current) return;
      artifactReadGeneration.current += 1;
      artifactReadBusy.current = false;
      setArtifactBusy(false);
    };
  }, [canReadArtifact]);
  const diagnosticOutput = presentation.tone === "danger" || presentation.tone === "warning";
  const shortOutput = !diagnosticOutput && !compactCompletedRead && !questionReceipt && canReadArtifact
    && tool.artifactPersistedBytes !== undefined
    && tool.artifactPersistedBytes <= SHORT_OUTPUT_BYTES;
  useEffect(() => {
    if (!shortOutput) return;
    return readArtifact({ kind: "byte_slice", offset: 0, limit: SHORT_OUTPUT_BYTES });
  }, [shortOutput, tool.key, tool.artifactRef, readArtifact]);
  const readingPage = artifactPage?.selector.kind === "search_literal" ? artifactReadingPosition.current?.page : artifactPage;
  const completeInlineOutput = isCompleteInlineOutput(readingPage, tool) ? readingPage : undefined;
  const compactSearch = completeInlineOutput !== undefined
    && presentation.tone === "success" && tool.artifactHasMore !== true;
  const returnToReading = () => {
    const position = artifactReadingPosition.current;
    if (artifactReadBusy.current || position === undefined) return;
    artifactScrollTop.current = position.scrollTop;
    artifactRestoreFocus.current = position.readerOpen && position.page !== undefined;
    setArtifactReaderOpen(position.readerOpen);
    if (position.page !== undefined) cacheArtifactPage(artifactPageCache.current, position.page);
    setArtifactPage(position.page);
    setArtifactPreviousSelectors(position.previousSelectors);
    setArtifactError(false);
    artifactReadingPosition.current = undefined;
    if ((!position.readerOpen || position.page === undefined) && !shortOutput) {
      setArtifactReaderOpen(false);
      artifactReaderToggle.current?.focus();
    }
  };
  const artifactSearchForm = (
    <form onSubmit={(event) => {
      event.preventDefault();
      const query = artifactQuery.trim();
      if (query !== "") void readArtifact({ kind: "search_literal", query, startOffset: 0, maxMatches: 20, contextLines: 2 });
    }}>
      <TextField ref={artifactSearchInput} label={t("searchSavedOutput")} labelHidden containerClassName="tool-artifact-search-field"
        maxLength={512} value={artifactQuery} onChange={(event) => setArtifactQuery(event.target.value)} />
      <Button type="submit" variant="quiet" disabled={artifactQuery.trim() === ""} busy={artifactBusy}>{t("search")}</Button>
    </form>
  );
  const searching = artifactPage?.selector.kind === "search_literal";
  const showPageContext = artifactPage !== undefined && (!compactSearch || searching);
  const showArtifactPage = artifactPage !== undefined && (shortOutput || artifactReaderOpen);
  const artifactResults = <div className="tool-artifact-reader" aria-busy={artifactBusy}>
    {showPageContext ? <header className="tool-artifact-context">
      <div>
        <strong>{artifactPage.selector.kind === "search_literal"
          ? t("savedOutputSearchTitle", { query: artifactPage.selector.query }) : t("savedOutput")}</strong>
        <small>{artifactPageRange(artifactPage, tool.artifactPersistedBytes, t)}
          {searching ? ` · ${t("savedOutputMatchCount", { count: artifactPage.matchCount })}` : ""}</small>
      </div>
      {searching ? <Button type="button" variant="quiet" disabled={artifactBusy}
        onClick={returnToReading}>{t("returnToOutput")}</Button> : null}
    </header> : null}
    <div className="tool-artifact-content" ref={artifactContent} tabIndex={artifactPage === undefined ? undefined : 0}
      aria-label={t("savedOutputReadingArea")}>
      {artifactPage === undefined ? null : artifactPage.bodyEncoding !== "utf8" ? <p>{t("savedOutputBinary")}</p>
        : artifactPage.selector.kind === "search_literal" && artifactPage.matchCount === 0 ? (
          <p role="status">{t(artifactPage.nextSelector !== undefined ? "savedOutputNoMatchesInRange"
            : artifactPreviousSelectors.length > 0 ? "savedOutputNoMoreMatches" : "savedOutputNoMatches")}</p>
        ) : artifactPage.body === "" ? <p>{t("noToolOutput")}</p> : (
          <HighlightedCode text={artifactPage.body} ariaLabel={`${tool.toolName} saved output ${searching ? "search results" : "page"}`} />
        )}
    </div>
    {artifactBusy ? <small role="status">{t("savedOutputLoading")}</small> : null}
    {artifactPage === undefined || (!showPageContext && artifactPage.nextSelector === undefined) ? null : (
      <nav className="tool-artifact-pagination" aria-label={t("savedOutputNavigation")}>
        {artifactPreviousSelectors.length === 0 ? null : <Button type="button" variant="quiet" disabled={artifactBusy}
          onClick={() => void readArtifact(artifactPreviousSelectors[artifactPreviousSelectors.length - 1]!, "previous")}>{t("previousOutputPage")}</Button>}
        {artifactPage.nextSelector === undefined ? artifactPage.eof ? <small>{t(searching ? "savedOutputSearchEnd" : "savedOutputEnd")}</small> : null : (
          <Button type="button" variant="quiet" busy={artifactBusy}
            onClick={() => void readArtifact(artifactPage.nextSelector!, "next")}>{t("nextOutputPage")}</Button>
        )}
      </nav>
    )}
    {artifactError ? <small role="alert">{t("artifactReadFailed")}</small> : null}
  </div>;
  const artifactSearchButton = (
    <Tooltip label={t("searchSavedOutput")}>
      <IconButton ref={artifactSearchToggle} type="button" className="tool-artifact-search-toggle"
        icon={<Icon name="search" />} aria-label={t("searchSavedOutput")}
        aria-expanded={artifactSearchOpen} aria-controls={artifactSearchId}
        onClick={() => setArtifactSearchOpen((open) => !open)} />
    </Tooltip>
  );
  const artifactSearchPanel = artifactSearchOpen ? (
    <div id={artifactSearchId} className="tool-artifact-search-panel" onKeyDown={(event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setArtifactSearchOpen(false);
      artifactSearchToggle.current?.focus();
    }}>
      {artifactSearchForm}
    </div>
  ) : null;
  const artifactPresentation = tool.artifactAvailability === undefined
    ? undefined
    : presentArtifactStatus(tool.artifactAvailability, t);
  const showSummary = !compactCompletedRead && presentation.summary !== undefined && !(presentation.tone === "success"
    && (shortOutput || (canReadArtifact && showArtifactPage && !questionReceipt && presentation.detailKind !== "diff")));
  return (
    <article
      className={`tool-card tool-tone-${presentation.tone}${presentation.detailKind === undefined || compactCompletedRead && !artifactReaderOpen ? " tool-card-compact" : ""}`}
      data-display-id={displayId}
      aria-label={`${presentation.displayName}: ${presentation.status}`}
    >
      <header className="tool-card-header">
        <span className="tool-status-icon" aria-hidden="true"><Icon name={toneIcon(presentation.tone)} /></span>
        <span className="tool-card-heading">
          <strong>{presentation.displayName}</strong>
          {presentation.tone === "success" ? null : <small>{presentation.status}</small>}
        </span>
        <span className="tool-card-meta">
          {duration === undefined ? null : <small aria-label={t("toolExecutionElapsed")}>{duration}</small>}
          {tool.risk === undefined ? null : <small className="tool-risk">{tool.risk} risk</small>}
        </span>
      </header>
      {onCancelExecution === undefined ? null : (
        <Button type="button" variant="quiet" busy={cancelling} onClick={onCancelExecution}>{t("toolNameStopCommand")}</Button>
      )}
      {presentation.input === undefined && !showSummary ? null : (
        <div className="tool-card-main">
          {presentation.input === undefined ? null : (
            <ToolInput key={tool.key} toolName={tool.toolName} input={presentation.input} language={inputLanguage} />
          )}
          {!showSummary ? null : (
            <div className="tool-result">
              <small>{t(presentation.tone === "danger" || presentation.tone === "warning" ? "toolStatusLabel" : "toolResultLabel")}</small>
              <p>{presentation.summary}</p>
            </div>
          )}
        </div>
      )}
      {canReadArtifact ? (
        <section className="tool-artifact-retrieval" aria-label={t("savedOutput")}>
          <div className="tool-artifact-toolbar">
            {!shortOutput || artifactPage === undefined ? (
              <Button ref={artifactReaderToggle} type="button" variant="quiet" className="tool-artifact-disclosure"
                leadingIcon={<Icon name={showArtifactPage ? "chevron-up" : "chevron-down"} />}
                aria-expanded={showArtifactPage} aria-controls={artifactReaderId}
                busy={artifactBusy && artifactPage === undefined}
                onClick={() => {
                  if (showArtifactPage) {
                    artifactScrollTop.current = artifactContent.current?.scrollTop ?? 0;
                    setArtifactReaderOpen(false);
                    setArtifactSearchOpen(false);
                  } else {
                    setArtifactReaderOpen(true);
                    if (artifactPage === undefined) void readArtifact({ kind: "byte_slice", offset: 0, limit: compactCompletedRead ? SHORT_OUTPUT_BYTES : 16_384 });
                    else artifactRestoreFocus.current = true;
                  }
                }}>{t(showArtifactPage ? "collapseSavedOutput" : "viewSavedOutput")}</Button>
            ) : null}
            {showArtifactPage && !compactSearch ? <Button type="button" variant="quiet" disabled={artifactBusy}
              onClick={() => void readArtifact({ kind: "byte_slice", offset: 0, limit: 16_384 })}>{t("restartSavedOutput")}</Button> : null}
            {artifactSearchButton}
          </div>
          {artifactSearchPanel}
          <div id={artifactReaderId} className="tool-artifact-display">
            {!diagnosticOutput && !compactCompletedRead && !shortOutput && !showArtifactPage && presentation.detailKind === "output" ? (
              <ToolOutputPanel key={tool.key} toolName={tool.toolName} text={presentation.detailText ?? ""}
                language={presentation.detailLanguage} streaming={presentation.streaming} allowExpand={false} />
            ) : null}
            {shortOutput || artifactReaderOpen ? artifactResults : null}
          </div>
        </section>
      ) : presentation.detailKind === "output" ? (
        <ToolOutputPanel key={tool.key} toolName={tool.toolName} text={presentation.detailText ?? ""}
          language={presentation.detailLanguage} streaming={presentation.streaming} />
      ) : null}
      {presentation.detailKind === undefined || presentation.detailKind === "output" ? null : (
        <details className="tool-details">
          <summary>
            <span>{presentation.detailLabel}</span>
            <small>{detailMetadata(
              presentation.detailLanguage,
              presentation.detailText?.split("\n").length ?? 0,
            )}</small>
          </summary>
          <div className="tool-card-body">
            {questionReceipt && tool.input !== undefined ? <>
              <small>{t("questionOriginalInput")}</small>
              <HighlightedCode text={tool.input} language="json" ariaLabel={t("questionOriginalInput")} />
              <small>{t("questionOriginalReceipt")}</small>
            </> : null}
            {presentation.detailKind === "diff" ? (
              <DiffViewer diff={presentation.detailText ?? ""} />
            ) : (
              <HighlightedCode
                text={boundedDetail.text}
                language={presentation.detailLanguage}
                ariaLabel={`${tool.toolName} ${presentation.detailKind === "raw" ? "raw details" : "output"}`}
              />
            )}
            {boundedDetail.omittedLines > 0 ? (
              <small>
                {boundedDetail.omittedLines} output line
                {boundedDetail.omittedLines === 1 ? "" : "s"} omitted from this view.
              </small>
            ) : null}
          </div>
        </details>
      )}
      {!canReadArtifact && artifactPresentation !== undefined ? (
        <section
          className={`tool-artifact-status tool-status-${artifactPresentation.status}`}
          data-artifact-product-status={artifactPresentation.status}
          aria-label={artifactPresentation.label}
        >
          <strong>{artifactPresentation.message}</strong>
          <small><strong>{t("nextStepLabel")}:</strong> {artifactPresentation.nextStep}</small>
          <details>
            <summary>{t("showDetails")}</summary>
            <p><strong>{t("technicalDetailsLabel")}:</strong> {artifactPresentation.technicalDetail}</p>
          </details>
        </section>
      ) : null}
    </article>
  );
}

function isCompleteInlineOutput(page: ToolArtifactPage | undefined, tool: ToolView): page is ToolArtifactPage {
  return page !== undefined && page.artifactRef === tool.artifactRef && page.bodyEncoding === "utf8"
    && page.selector.kind === "byte_slice" && page.selector.offset === 0 && page.eof
    && page.nextSelector === undefined && page.returnedBytes === tool.artifactPersistedBytes
    && page.returnedBytes <= SHORT_OUTPUT_BYTES;
}

function artifactPageRange(page: ToolArtifactPage, totalBytes: number | undefined, t: Translate): string {
  const selector = page.selector;
  if (selector.kind === "line_page") {
    return t("savedOutputLineStart", { start: selector.startLine + 1 });
  }
  if (selector.kind === "search_literal") {
    const end = page.nextSelector?.kind === "search_literal" ? page.nextSelector.startOffset
      : page.eof ? totalBytes : undefined;
    return end === undefined ? t("savedOutputSearchFrom", { start: selector.startOffset + 1 })
      : t("savedOutputByteRange", { start: Math.min(selector.startOffset + 1, end), end });
  }
  return t("savedOutputByteRange", {
    start: selector.offset + (page.returnedBytes > 0 ? 1 : 0), end: selector.offset + page.returnedBytes,
  });
}

function artifactSelectorKey(selector: ToolArtifactSelector): string {
  switch (selector.kind) {
    case "byte_slice": return JSON.stringify([selector.kind, selector.offset, selector.limit]);
    case "line_page": return JSON.stringify([selector.kind, selector.startLine, selector.lineCount]);
    case "search_literal": return JSON.stringify([selector.kind, selector.query, selector.startOffset, selector.maxMatches, selector.contextLines]);
  }
}

function cacheArtifactPage(cache: Map<string, CachedArtifactPage>, page: ToolArtifactPage, readingPage?: ToolArtifactPage) {
  const key = artifactSelectorKey(page.selector);
  const bodyBytes = new TextEncoder().encode(page.body).length;
  cache.delete(key);
  if (bodyBytes > ARTIFACT_CACHE_MAX_BYTES) return;
  cache.set(key, { page, bodyBytes });
  const readingKey = readingPage === undefined ? undefined : artifactSelectorKey(readingPage.selector);
  let retainedBytes = Array.from(cache.values()).reduce((bytes, entry) => bytes + entry.bodyBytes, 0);
  for (const [cachedKey, entry] of cache) {
    if (cache.size <= ARTIFACT_CACHE_MAX_PAGES && retainedBytes <= ARTIFACT_CACHE_MAX_BYTES) break;
    if (cachedKey === key || cachedKey === readingKey) continue;
    cache.delete(cachedKey);
    retainedBytes -= entry.bodyBytes;
  }
  if (cache.size > ARTIFACT_CACHE_MAX_PAGES || retainedBytes > ARTIFACT_CACHE_MAX_BYTES) cache.delete(key);
}

function ToolInput({ toolName, input, language }: {
  readonly toolName: string;
  readonly input: string;
  readonly language?: string;
}) {
  const { t } = useLocale();
  const [expanded, setExpanded] = useState(false);
  const preview = language === undefined ? { text: input, truncated: false } : outputPreview(input);
  return (
    <div className={`tool-input${language === undefined ? " is-parameters" : " is-command"}`}>
      <HighlightedCode text={expanded ? input : preview.text} language={language} ariaLabel={`${toolName} input`} />
      {preview.truncated ? (
        <Button type="button" variant="quiet" aria-expanded={expanded} onClick={() => setExpanded(!expanded)}>
          {t(expanded ? "toolCollapseCommand" : "toolExpandCommand")}
        </Button>
      ) : null}
    </div>
  );
}

function ToolOutputPanel({
  toolName,
  text,
  language,
  streaming,
  allowExpand = true,
}: {
  readonly toolName: string;
  readonly text: string;
  readonly language?: string;
  readonly streaming: boolean;
  readonly allowExpand?: boolean;
}) {
  const { t } = useLocale();
  const [expanded, setExpanded] = useState(streaming);
  useLayoutEffect(() => {
    setExpanded(streaming);
  }, [streaming, toolName]);
  const bounded = boundedOutput(text);
  const preview = outputPreview(bounded.text);
  const structured = language === "json";
  const expandable = structured || preview.truncated;
  const showExpanded = allowExpand && expanded;
  const visibleText = showExpanded || !expandable ? bounded.text : preview.text;
  const showOutput = visibleText.trim() !== "";
  const lineCount = text.split("\n").length;
  const metadata = detailMetadata(language, lineCount);
  const outputLabel = t(toolName === "read_file" ? "preview" : "toolOutputLabel");
  return (
    <section
      className={`tool-output-section${showExpanded ? " is-expanded" : ""}`}
      aria-label={`${toolName} output`}
    >
      {expandable && allowExpand ? (
        <Button
          type="button"
          variant="quiet"
          className="tool-output-disclosure"
          aria-label={`${expanded ? "Collapse" : "Expand"} ${toolName} output`}
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          <span className="tool-output-disclosure-title">
            <Icon name={expanded ? "chevron-up" : "chevron-down"} />
            <strong>{outputLabel}</strong>
          </span>
          <small>{metadata}</small>
        </Button>
      ) : (
        <header>
          <strong>{outputLabel}</strong>
          <small>{metadata}</small>
        </header>
      )}
      {showOutput ? (
        <HighlightedCode
          text={visibleText}
          language={language}
          ariaLabel={`${toolName} output content`}
        />
      ) : null}
      {showExpanded && bounded.omittedLines > 0 ? (
        <small>{bounded.omittedLines} output line{bounded.omittedLines === 1 ? "" : "s"} omitted from this view.</small>
      ) : null}
    </section>
  );
}

export function presentTool(tool: ToolView, t: Translate = translateEnglish): ToolPresentation {
  const receipt = tool.toolName === "request_user_input" && statusTone(tool.status) === "success"
    ? tool.userInputReceipt : undefined;
  if (receipt !== undefined) {
    const submitted = receipt.decision === "submitted";
    return {
      displayName: humanizeToolName(tool.toolName, t),
      status: t(submitted ? "toolStatusCompleted" : receipt.decision === "declined" ? "questionDeclined" : "toolStatusCancelled"),
      tone: submitted ? "success" : "warning",
      summary: submitted ? t("questionReceiptSubmitted", { count: receipt.answeredCount })
        : t(receipt.decision === "declined" ? "questionReceiptDeclined" : "questionReceiptCancelled"),
      detailKind: tool.text !== "" || tool.input !== undefined ? "raw" : undefined,
      detailLabel: t("questionReceiptDetails"),
      detailText: tool.text,
      detailLanguage: "json",
      streaming: false,
    };
  }
  const structured = parseStructuredOutput(tool.text);
  const status = tool.status ?? structured?.status ?? "recorded";
  const tone = statusTone(status);
  const diff = isUnifiedDiff(tool.text);
  const outputPresentation = diff
    ? undefined
    : presentOutput(
      structured === undefined ? tool.text : structured.output,
      tool.toolName,
      tone,
      structured?.language,
    );
  const summary = structured?.summary
    ?? (diff ? t("toolChangesReady") : tool.toolName === "read_file" && tone === "success" && outputPresentation !== undefined
      ? t("toolOutputPreview") : outputPresentation?.summary);
  const detailKind = diff
    ? "diff" as const
    : structured?.hasError
      ? "raw" as const
      : outputPresentation?.detailText !== undefined
      ? "output" as const
      : undefined;
  return {
    displayName: humanizeToolName(tool.toolName, t),
    status: humanizeStatus(status, t),
    tone,
    summary,
    input: tool.input ?? structured?.input,
    detailKind,
    detailLabel: detailKind === "diff" ? t("toolReviewChanges") : detailKind === "raw" ? t("toolRawDetails") : undefined,
    detailText: detailKind === "raw"
      ? tool.text
      : detailKind === undefined ? undefined : outputPresentation?.detailText ?? tool.text,
    detailLanguage: detailKind === "raw"
      ? "json"
      : detailKind === "output"
        ? outputPresentation?.language
        : undefined,
    streaming: isStreamingStatus(tool.status ?? structured?.status),
  };
}

function parseStructuredOutput(text: string): StructuredOutput | undefined {
  const trimmed = text.trim();
  if (!trimmed.startsWith("{") || !trimmed.endsWith("}")) return undefined;
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return undefined;
  }
  const root = asRecord(parsed);
  if (root === undefined) return undefined;
  if (!("status" in root || "content" in root || "error" in root || "meta" in root)) {
    return undefined;
  }
  const error = asRecord(root.error);
  const meta = asRecord(root.meta);
  const details = asRecord(meta?.details);
  const call = asRecord(details?.call);
  const status = stringValue(root.status);
  const content = stringValue(root.content);
  const summary = firstText(
    stringValue(error?.message),
    stringValue(details?.summary),
  );
  return {
    status,
    summary: summary === undefined ? undefined : boundedSummary(summary),
    input: firstText(stringValue(call?.command), stringValue(call?.summary)),
    output: error === undefined ? content : undefined,
    language: inferOutputLanguage(
      stringValue(details?.language),
      firstText(stringValue(details?.path), stringValue(call?.path)),
      content,
    ),
    hasError: error !== undefined,
  };
}

function presentOutput(
  content: string | undefined,
  toolName: string,
  tone: ToolTone,
  explicitLanguage?: string,
): OutputPresentation | undefined {
  const trimmed = content?.trim() ?? "";
  if (trimmed === "") return fallbackSummary(tone);

  const json = parseJson(trimmed);
  if (json.parsed) {
    if (isEmptyJsonValue(json.value)) {
      return { summary: emptyOutputSummary(toolName) };
    }
    const formatted = JSON.stringify(json.value, null, 2);
    return {
      summary: jsonOutputSummary(json.value, toolName),
      detailText: formatted,
      language: explicitLanguage ?? "json",
    };
  }

  const lines = trimmed.split("\n");
  const firstLine = lines.find((line) => line.trim() !== "")?.trim();
  const language = explicitLanguage ?? inferContentLanguage(content);
  const summary = firstLine === undefined
    ? fallbackSummary(tone)?.summary
    : isStructuralOnlyLine(firstLine)
      ? `${lines.length} lines of output.`
      : textOutputSummary(lines);
  if (summary === undefined) return undefined;
  return {
    summary,
    detailText: lines.length > 1 || language !== undefined ? trimmed : undefined,
    language,
  };
}

function parseJson(value: string): { readonly parsed: true; readonly value: unknown } | { readonly parsed: false } {
  if (!/^[\[{]/.test(value) && value !== "null") return { parsed: false };
  try {
    return { parsed: true, value: JSON.parse(value) as unknown };
  } catch {
    return { parsed: false };
  }
}

function isEmptyJsonValue(value: unknown): boolean {
  if (value === null) return true;
  if (typeof value === "string") return value.trim() === "";
  if (Array.isArray(value)) return value.length === 0;
  const record = asRecord(value);
  return record !== undefined && Object.keys(record).length === 0;
}

function emptyOutputSummary(toolName: string): string {
  const normalized = toolName.toLocaleLowerCase();
  if (/grep|search|find/.test(normalized)) return "No matches found.";
  if (/glob|list|(?:^|[_-])ls(?:$|[_-])|directory/.test(normalized)) return "No entries found.";
  return "Completed with no output.";
}

function jsonOutputSummary(value: unknown, toolName: string): string {
  if (Array.isArray(value)) {
    const matches = /grep|search|find/.test(toolName.toLocaleLowerCase());
    const noun = matches
      ? value.length === 1 ? "match" : "matches"
      : value.length === 1 ? "item" : "items";
    return `${value.length} ${noun}.`;
  }
  const record = asRecord(value);
  if (record !== undefined) {
    const fields = Object.keys(record).length;
    return `${fields} field${fields === 1 ? "" : "s"} of structured output.`;
  }
  return "Structured output available.";
}

function isStructuralOnlyLine(value: string): boolean {
  return /^[\[\]\{\},]+$/.test(value);
}

function textOutputSummary(lines: readonly string[]): string {
  const lineCount = lines.length;
  if (lineCount > 1) return `${lineCount} lines of output.`;
  return boundedSummary(lines[0]?.trim() ?? "");
}

function boundedOutput(text: string): { readonly text: string; readonly omittedLines: number } {
  const lines = text.split("\n");
  return {
    text: lines.slice(0, MAX_VISIBLE_OUTPUT_LINES).join("\n"),
    omittedLines: Math.max(0, lines.length - MAX_VISIBLE_OUTPUT_LINES),
  };
}

function outputPreview(text: string): OutputPreview {
  const lines = text.split("\n");
  const linePreview = lines.slice(0, OUTPUT_PREVIEW_LINES).join("\n");
  if (linePreview.length > OUTPUT_PREVIEW_CHARACTERS) {
    return {
      text: `${linePreview.slice(0, OUTPUT_PREVIEW_CHARACTERS - 1).trimEnd()}…`,
      truncated: true,
    };
  }
  return { text: linePreview, truncated: lines.length > OUTPUT_PREVIEW_LINES };
}

function toolInputLanguage(toolName: string): string | undefined {
  return toolName === "exec_command" || /bash|shell|terminal/i.test(toolName) ? "bash" : undefined;
}

function inferOutputLanguage(
  explicit: string | undefined,
  path: string | undefined,
  content: string | undefined,
): string | undefined {
  return normalizeLanguage(explicit)
    ?? languageFromPath(path)
    ?? inferContentLanguage(content);
}

function inferContentLanguage(content: string | undefined): string | undefined {
  if (content === undefined) return undefined;
  const trimmed = content.trim();
  if (!((trimmed.startsWith("{") && trimmed.endsWith("}"))
    || (trimmed.startsWith("[") && trimmed.endsWith("]")))) return undefined;
  try {
    JSON.parse(trimmed);
    return "json";
  } catch {
    return undefined;
  }
}

function languageFromPath(path: string | undefined): string | undefined {
  const extension = path?.toLocaleLowerCase().match(/\.([a-z0-9]+)$/)?.[1];
  return normalizeLanguage(extension);
}

function normalizeLanguage(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  const normalized = value.trim().toLocaleLowerCase();
  const aliases: Readonly<Record<string, string>> = {
    c: "c",
    cc: "cpp",
    cpp: "cpp",
    cs: "csharp",
    css: "css",
    diff: "diff",
    go: "go",
    h: "c",
    hpp: "cpp",
    html: "xml",
    java: "java",
    js: "javascript",
    javascript: "javascript",
    json: "json",
    jsx: "javascript",
    kt: "kotlin",
    kotlin: "kotlin",
    lua: "lua",
    md: "markdown",
    markdown: "markdown",
    php: "php",
    py: "python",
    python: "python",
    rb: "ruby",
    rs: "rust",
    rust: "rust",
    sh: "bash",
    shell: "bash",
    sql: "sql",
    swift: "swift",
    toml: "ini",
    ts: "typescript",
    tsx: "typescript",
    typescript: "typescript",
    xml: "xml",
    yaml: "yaml",
    yml: "yaml",
  };
  return aliases[normalized];
}

function detailMetadata(language: string | undefined, lineCount: number): string {
  const lines = `${lineCount} line${lineCount === 1 ? "" : "s"}`;
  return language === undefined ? lines : `${language.charAt(0).toUpperCase()}${language.slice(1)} · ${lines}`;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : undefined;
}

function firstText(...values: Array<string | undefined>): string | undefined {
  return values.find((value) => value !== undefined);
}

function fallbackSummary(tone: ToolTone): OutputPresentation | undefined {
  switch (tone) {
    case "danger": return { summary: "The tool did not complete successfully." };
    case "warning": return { summary: "The tool needs attention." };
    case "success":
    case "info":
    case "neutral":
      return undefined;
  }
}

function boundedSummary(value: string): string {
  if (value.length <= MAX_SUMMARY_CHARACTERS) return value;
  return `${value.slice(0, MAX_SUMMARY_CHARACTERS - 1).trimEnd()}…`;
}

const TOOL_LABEL_KEYS = {
  exec_command: "toolNameCommand", exec_read: "toolNameReadCommand", exec_wait: "toolNameWaitCommand",
  exec_input: "toolNameInputCommand", exec_resize: "toolNameResizeCommand", exec_cancel: "toolNameStopCommand",
  read_file: "toolNameReadFile", write_file: "toolNameWriteFile", edit_file: "toolNameEditFile",
  delete_file: "toolNameDeleteFile", ls: "toolNameListFiles", glob: "toolNameFindFiles", grep: "toolNameSearchText",
  apply_changeset: "toolNameApplyChanges", vcs_inspect: "toolNameInspectChanges", read_tool_artifact: "toolNameReadOutput",
  request_user_input: "toolNameAskQuestion", request_plan_review: "toolNameReviewPlan", start_task: "toolNameStartTask",
  spawn_agent: "toolNameStartAgent", spawn_agents: "toolNameStartAgents", wait_agent: "toolNameWaitAgent",
  read_agent_result: "toolNameReadAgent", message_agent: "toolNameMessageAgent", list_agents: "toolNameListAgents",
  cancel_agent: "toolNameCancelAgent", close_agent: "toolNameCloseAgent", integrate_agent_changes: "toolNameIntegrateAgent",
} as const;

const STATUS_LABEL_KEYS = {
  recorded: "toolStatusRecorded", pending: "toolStatusPending", requested: "toolStatusRequested",
  starting: "toolStatusStarting", running: "toolStatusRunning", streaming: "toolStatusRunning", progress: "toolStatusRunning",
  waiting_for_approval: "toolStatusApproval", awaiting_approval: "toolStatusApproval", approval: "toolStatusApproval",
  awaiting_user_input: "toolStatusInput", approved: "toolStatusApproved", denied: "toolStatusDenied",
  completed: "toolStatusCompleted", succeeded: "toolStatusSucceeded", success: "toolStatusSucceeded", ok: "toolStatusSucceeded",
  failed: "toolStatusFailed", error: "toolStatusFailed", cancelled: "toolStatusCancelled", interrupted: "toolStatusInterrupted",
  paused: "toolStatusPaused", blocked: "toolStatusBlocked", expired: "toolStatusExpired", timed_out: "toolStatusTimeout",
  cleanup_incomplete: "toolStatusCleanup", warning: "toolStatusWarning",
} as const;

function humanizeToolName(value: string, t: Translate): string {
  if (!Object.hasOwn(TOOL_LABEL_KEYS, value)) return value;
  const key = TOOL_LABEL_KEYS[value as keyof typeof TOOL_LABEL_KEYS];
  return key === undefined ? value : t(key);
}

function humanizeStatus(value: string, t: Translate): string {
  if (!Object.hasOwn(STATUS_LABEL_KEYS, value)) return value;
  const key = STATUS_LABEL_KEYS[value as keyof typeof STATUS_LABEL_KEYS];
  return key === undefined ? value : t(key);
}

function statusTone(value?: string): ToolTone {
  const status = value ?? "";
  if (["failed", "error", "timed_out", "cleanup_incomplete"].includes(status)) return "danger";
  if (["denied", "cancelled", "blocked", "warning", "expired", "interrupted", "paused"].includes(status)) return "warning";
  if (["ok", "success", "succeeded", "completed", "approved"].includes(status)) return "success";
  if (["streaming", "running", "progress", "pending", "starting", "requested", "waiting_for_approval", "awaiting_approval", "approval", "awaiting_user_input"].includes(status)) return "info";
  return "neutral";
}

function isStreamingStatus(value?: string): boolean {
  return ["streaming", "running", "progress", "starting"].includes(value ?? "");
}

function toneIcon(tone: ToolTone): IconName {
  if (tone === "success") return "check";
  if (tone === "warning" || tone === "danger") return "warning";
  return "more";
}
