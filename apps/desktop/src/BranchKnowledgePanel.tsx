import { useEffect, useRef, useState } from "react";

import type { DesktopBridge } from "./bridge";
import type { BranchKnowledgePreview, BranchLineage, BranchLink } from "./features/conversation/branchTypes";
import { useLocale } from "./i18n";
import type { CatalogEntry, ConversationRecoveryCommandReceipt } from "./types";
import { Button, Select, TextField } from "./ui/primitives";

export function BranchKnowledgePanel({ bridge, workspaceId, sessionId, onOpen, onImported }: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly sessionId: string;
  readonly onOpen: (sessionRef: string, sessionId: string, title?: string) => Promise<void>;
  readonly onImported: (receipt: ConversationRecoveryCommandReceipt) => void;
}) {
  const { t } = useLocale();
  const [lineage, setLineage] = useState<BranchLineage>();
  const [sources, setSources] = useState<CatalogEntry[]>([]);
  const [cursor, setCursor] = useState<string>();
  const [query, setQuery] = useState("");
  const [catalogQuery, setCatalogQuery] = useState("");
  const [sourceKey, setSourceKey] = useState("");
  const [preview, setPreview] = useState<BranchKnowledgePreview>();
  const [pointIndex, setPointIndex] = useState(0);
  const [loading, setLoading] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState(false);
  const [imported, setImported] = useState<"new" | "existing">();
  const owner = useRef(0);
  const catalogRequest = useRef(0);
  const previewRequest = useRef(0);
  const key = (entry: CatalogEntry) => JSON.stringify([entry.sessionRef, entry.sessionId]);

  const loadSources = async (search: string, next?: string) => {
    const request = ++catalogRequest.current;
    const epoch = owner.current;
    setLoading(true);
    setError(false);
    try {
      const page = await bridge.catalog(workspaceId, { limit: 50, query: search || undefined, cursor: next });
      if (epoch !== owner.current || request !== catalogRequest.current) return;
      const entries = page.entries.filter((entry) => entry.sourceState === "ready" && entry.sessionId !== undefined);
      setSources((current) => next === undefined ? entries : [...current, ...entries.filter((entry) => !current.some((existing) => key(existing) === key(entry)))]);
      setCursor(page.nextCursor);
      setCatalogQuery(search);
    } catch {
      if (epoch === owner.current && request === catalogRequest.current) setError(true);
    } finally {
      if (epoch === owner.current && request === catalogRequest.current) setLoading(false);
    }
  };

  useEffect(() => {
    const epoch = ++owner.current;
    setLineage(undefined);
    setSources([]);
    setPreview(undefined);
    setSourceKey("");
    setImported(undefined);
    setImporting(false);
    setPreviewing(false);
    void bridge.branchLineage(workspaceId, sessionId).then((value) => {
      if (epoch === owner.current) setLineage(value);
    }).catch(() => { if (epoch === owner.current) setError(true); });
    void loadSources("");
    return () => { owner.current += 1; };
    // Each mounted target owns its pending reads and receipts; search is explicitly submitted.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bridge, workspaceId, sessionId]);

  const chooseSource = async (value: string) => {
    const source = sources.find((entry) => key(entry) === value);
    const request = ++previewRequest.current;
    const epoch = owner.current;
    setSourceKey(value);
    setPreview(undefined);
    setPointIndex(0);
    setImported(undefined);
    setError(false);
    if (source?.sessionId === undefined) { setPreviewing(false); return; }
    setPreviewing(true);
    try {
      const view = await bridge.branchKnowledgePreview(workspaceId, sessionId, {
        sourceSessionRef: source.sessionRef, sourceSessionId: source.sessionId,
      });
      if (epoch === owner.current && request === previewRequest.current) setPreview(view);
    } catch {
      if (epoch === owner.current && request === previewRequest.current) setError(true);
    } finally {
      if (epoch === owner.current && request === previewRequest.current) setPreviewing(false);
    }
  };

  const importPoint = async () => {
    const point = preview?.points[pointIndex];
    if (preview === undefined || point === undefined || importing) return;
    const epoch = owner.current;
    const request = previewRequest.current;
    setImporting(true);
    setError(false);
    setImported(undefined);
    try {
      const receipt = await bridge.commandConversationRecovery(workspaceId, { sessionId, action: {
        kind: "import_branch_knowledge", selection: {
          sourceSessionRef: preview.sourceSessionRef, sourceSessionId: preview.sourceSessionId,
          sourceTurnDigest: point.sourceTurnDigest, sourceMessageId: point.sourceMessageId,
          sourceTextSha256: point.sourceTextSha256, summarySha256: point.summarySha256,
        },
      } });
      if (epoch !== owner.current) return;
      if (receipt.sessionId !== sessionId || receipt.action !== "import_branch_knowledge" || receipt.branchKnowledge === undefined) throw new Error("unexpected branch receipt");
      onImported(receipt);
      if (request === previewRequest.current) setImported(receipt.branchKnowledge.alreadyImported ? "existing" : "new");
    } catch {
      if (epoch === owner.current && request === previewRequest.current) setError(true);
    } finally {
      if (epoch === owner.current) setImporting(false);
    }
  };

  const open = (link: BranchLink) => {
    const epoch = owner.current;
    void onOpen(link.sessionRef, link.sessionId, link.title ?? t("untitledConversation")).catch(() => {
      if (epoch === owner.current) setError(true);
    });
  };
  const point = preview?.points[pointIndex];
  return <section className="conversation-recovery-section branch-knowledge" aria-label={t("branchKnowledge")}>
    <header><div><h3>{t("branchKnowledge")}</h3><p>{t("branchKnowledgeDetail")}</p></div></header>
    {lineage !== undefined && <nav aria-label={t("branchRelations")}>
      {lineage.parent && <Button type="button" onClick={() => open(lineage.parent!)}>{t("parentBranch")}: {lineage.parent.title ?? t("untitledConversation")}</Button>}
      {lineage.children.map((child) => <Button type="button" key={JSON.stringify([child.sessionRef, child.sessionId])} onClick={() => open(child)}>{t("childBranch")}: {child.title ?? t("untitledConversation")}</Button>)}
      {lineage.parent == null && lineage.children.length === 0 && <p>{t("noRelatedBranches")}</p>}
      {lineage.unavailableCount > 0 && <p>{t("branchesUnavailable", { count: lineage.unavailableCount })}</p>}
    </nav>}
    <form onSubmit={(event) => { event.preventDefault(); void chooseSource(""); void loadSources(query); }}>
      <TextField label={t("searchKnowledgeSource")} value={query} onChange={(event) => setQuery(event.target.value)} />
      <Button type="submit" busy={loading}>{t("search")}</Button>
    </form>
    <Select label={t("knowledgeSource")} value={sourceKey} onChange={(event) => void chooseSource(event.target.value)}>
      <option value="">{t("selectKnowledgeSource")}</option>
      {sources.map((source) => <option key={key(source)} value={key(source)}>{source.title ?? t("untitledConversation")}</option>)}
    </Select>
    {cursor && <Button type="button" busy={loading} onClick={() => void loadSources(catalogQuery, cursor)}>{t("loadMore")}</Button>}
    {sourceKey && <Button type="button" busy={previewing} onClick={() => void chooseSource(sourceKey)}>{t("refreshReview")}</Button>}
    {previewing && <p role="status">{t("loading")}</p>}
    {preview !== undefined && preview.points.length === 0 && <p>{t("noCompletedConclusions")}</p>}
    {preview !== undefined && preview.points.length > 0 && <>
      <Select label={t("selectedConclusion")} value={pointIndex} onChange={(event) => { setPointIndex(Number(event.target.value)); setImported(undefined); previewRequest.current += 1; }}>
        {preview.points.map((candidate, index) => <option key={candidate.sourceMessageId} value={index}>{t("conclusionNumber", { number: index + 1 })}</option>)}
      </Select>
      {point && <div className="branch-knowledge-preview" tabIndex={0} role="region" aria-label={t("conclusionPreview")}><p>{point.summary}</p>{point.truncated && <p>{t("conclusionTruncated")}</p>}</div>}
      <p>{t("knowledgeUnverified")}</p>
      <Button type="button" busy={importing} onClick={() => void importPoint()}>{t(importing ? "importingConclusion" : "importConclusion")}</Button>
    </>}
    {imported && <p role="status">{t(imported === "existing" ? "conclusionAlreadyImported" : "conclusionImported")}</p>}
    {error && <p role="alert">{t("branchKnowledgeFailed")}</p>}
  </section>;
}
