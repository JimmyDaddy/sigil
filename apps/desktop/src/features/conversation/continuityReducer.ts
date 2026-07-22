import type {
  ConversationDisplayContent as SharedConversationDisplayContent,
  ConversationDisplayItem as SharedConversationDisplayItem,
  ConversationDisplayItemKind as SharedConversationDisplayKind,
  ConversationDisplayPage as SharedConversationDisplayPage,
  ConversationDisplaySource as SharedConversationDisplaySource,
  ConversationDisplayStatus as SharedConversationDisplayStatus,
} from "../../types";

export type DecimalSequence = string;
export type ConversationDisplayKind = SharedConversationDisplayKind;
export type ConversationDisplayStatus = SharedConversationDisplayStatus | "running" | "streaming";
export type ConversationDisplayContent = SharedConversationDisplayContent;
export type ConversationTerminalStatus = Extract<
  SharedConversationDisplayStatus,
  "succeeded" | "failed" | "cancelled" | "interrupted"
>;

type WithOptionalDurabilityMetadata<
  Content,
  Keys extends keyof Content,
> = Omit<Content, Keys> & Partial<Pick<Content, Keys>>;

type LiveConversationDisplayContent =
  | WithOptionalDurabilityMetadata<
    Extract<SharedConversationDisplayContent, { type: "message" }>,
    "imageAttachmentCount" | "truncated" | "originalContentBytes"
  >
  | WithOptionalDurabilityMetadata<
    Extract<SharedConversationDisplayContent, { type: "reasoning" }>,
    "truncated" | "originalContentBytes"
  >
  | WithOptionalDurabilityMetadata<
    Extract<SharedConversationDisplayContent, { type: "tool" }>,
    "truncated" | "originalContentBytes"
  >
  | WithOptionalDurabilityMetadata<
    Extract<SharedConversationDisplayContent, { type: "notice" }>,
    "truncated" | "originalContentBytes"
  >
  | Exclude<
    SharedConversationDisplayContent,
    { type: "message" | "reasoning" | "tool" | "notice" | "terminal" }
  >;

type NormalizedLiveConversationDisplayItem = Omit<
  LiveConversationDisplayItem,
  "content"
> & {
  content: Exclude<SharedConversationDisplayContent, { type: "terminal" }>;
};

type DurableConversationDisplaySource = Exclude<
  SharedConversationDisplaySource,
  "live_transient"
>;

export type DurableConversationDisplayItem = Omit<
  SharedConversationDisplayItem,
  "source" | "reconciles"
> & {
  source: DurableConversationDisplaySource;
  reconciles?: readonly string[];
};

export interface LiveConversationDisplayItem {
  provisionalId: string;
  runId: string;
  runSequence: DecimalSequence;
  kind: Exclude<ConversationDisplayKind, "terminal">;
  status: ConversationDisplayStatus;
  content: LiveConversationDisplayContent;
  /** Bounded live-only preview already projected by the allowlisted event bridge. */
  toolInput?: string;
}

export interface ConversationTerminalFrontier {
  runId: string;
  sessionStreamSequence: DecimalSequence;
  status: ConversationTerminalStatus;
}

export interface ConversationTerminalObservation {
  runId: string;
  status: ConversationTerminalStatus;
}

export type ConversationDisplayPage = Omit<
  SharedConversationDisplayPage,
  "items" | "terminalFrontier"
> & {
  items: readonly DurableConversationDisplayItem[];
  terminalFrontier?: ConversationTerminalFrontier;
};

type ConversationDisplayGapFact = ConversationDisplayPage["gapFacts"][number];
type ConversationLiveProvisionalAnchor = NonNullable<
  ConversationDisplayPage["liveProvisionalAnchor"]
>;

export type ConversationLifecycle =
  | "loading_transcript"
  | "checking_owner"
  | "attaching_run"
  | "live"
  | "finalizing"
  | "idle"
  | "read_only_recovery"
  | "read_only"
  | "error";

export interface ConversationOwnerState {
  foregroundRunId?: string;
  ownerRevision?: string;
}

export interface ConversationRecoveryState {
  code: string;
  message: string;
  canContinueReadOnly: boolean;
}

export interface ConversationContractError {
  code:
    | "request_scope_mismatch"
    | "invalid_sequence"
    | "conflicting_display_id"
    | "conflicting_provisional_id"
    | "invalid_reconciliation"
    | "duplicate_final"
    | "invalid_pagination"
    | "terminal_conflict";
  message: string;
}

export interface ConversationContinuityState {
  sessionId: string;
  requestScope?: string;
  lifecycle: ConversationLifecycle;
  transcriptLoaded: boolean;
  canonicalItems: ReadonlyMap<string, DurableConversationDisplayItem>;
  liveItems: ReadonlyMap<string, NormalizedLiveConversationDisplayItem>;
  reconciledIdentities: ReadonlySet<string>;
  reconciliationSuccessors: ReadonlyMap<string, string>;
  throughSessionStreamSequence?: DecimalSequence;
  totalItems?: DecimalSequence;
  nextCursor?: string;
  hasMore: boolean;
  gapFacts: readonly ConversationDisplayGapFact[];
  liveProvisionalAnchor?: ConversationLiveProvisionalAnchor;
  observedTerminal?: ConversationTerminalObservation;
  pendingTerminalRunId?: string;
  canonicalTerminal?: ConversationTerminalFrontier;
  owner: ConversationOwnerState;
  refreshState: "idle" | "needed" | "loading" | "failed";
  recovery?: ConversationRecoveryState;
  contractError?: ConversationContractError;
}

export type ConversationContinuityAction =
  | { type: "session_selected"; sessionId: string }
  | { type: "initial_page_received"; sessionId: string; page: ConversationDisplayPage }
  | { type: "initial_page_failed"; sessionId: string; message: string }
  | { type: "older_page_received"; sessionId: string; page: ConversationDisplayPage }
  | { type: "live_item_received"; sessionId: string; item: LiveConversationDisplayItem }
  | { type: "terminal_observed"; sessionId: string; terminal: ConversationTerminalObservation }
  | { type: "terminal_transport_observed"; sessionId: string; runId: string }
  | { type: "owner_probe_started"; sessionId: string }
  | {
    type: "owner_probe_resolved";
    sessionId: string;
    foregroundOwner?: { runId: string; ownerRevision: string };
  }
  | { type: "owner_probe_failed"; sessionId: string; message: string; canContinueReadOnly: boolean }
  | { type: "run_attached"; sessionId: string; runId: string; ownerRevision: string }
  | { type: "refresh_started"; sessionId: string }
  | { type: "refresh_page_received"; sessionId: string; page: ConversationDisplayPage }
  | {
    type: "refresh_failed";
    sessionId: string;
    message: string;
    canContinueReadOnly: boolean;
  }
  | { type: "recovery_retry_started"; sessionId: string }
  | { type: "continue_read_only"; sessionId: string };

export type ConversationTimelineItem =
  | { identity: string; source: "durable"; item: DurableConversationDisplayItem }
  | { identity: string; source: "live"; item: NormalizedLiveConversationDisplayItem };

export function createConversationContinuityState(sessionId: string): ConversationContinuityState {
  return {
    sessionId,
    lifecycle: "loading_transcript",
    transcriptLoaded: false,
    canonicalItems: new Map(),
    liveItems: new Map(),
    reconciledIdentities: new Set(),
    reconciliationSuccessors: new Map(),
    hasMore: false,
    gapFacts: [],
    owner: {},
    refreshState: "idle",
  };
}

export function reduceConversationContinuity(
  state: ConversationContinuityState,
  action: ConversationContinuityAction,
): ConversationContinuityState {
  if (action.type === "session_selected") {
    return action.sessionId === state.sessionId
      ? state
      : createConversationContinuityState(action.sessionId);
  }

  // Async work from a previously selected session must not mutate the new one.
  if (action.sessionId !== state.sessionId) return state;
  // A canonical contract violation is fail-closed. Only an explicit retry may
  // request a new canonical page, and only that valid page clears the error.
  if (
    state.contractError !== undefined
    && action.type !== "recovery_retry_started"
    && action.type !== "initial_page_received"
    && action.type !== "initial_page_failed"
  ) return state;

  switch (action.type) {
    case "initial_page_received":
      return receivePage(state, action.page, "initial");
    case "initial_page_failed":
      return {
        ...state,
        lifecycle: "error",
        recovery: {
          code: "canonical_display_unavailable",
          message: action.message,
          canContinueReadOnly: false,
        },
      };
    case "older_page_received":
      return receivePage(state, action.page, "older");
    case "refresh_page_received":
      return receivePage(state, action.page, "refresh");
    case "live_item_received":
      return receiveLiveItem(state, action.item);
    case "terminal_observed":
      return observeTerminal(state, action.terminal);
    case "terminal_transport_observed":
      return observeTerminalTransport(state, action.runId);
    case "owner_probe_started":
      return {
        ...state,
        lifecycle: "checking_owner",
        owner: {},
        recovery: undefined,
      };
    case "owner_probe_resolved":
      if (action.foregroundOwner === undefined) {
        return {
          ...state,
          lifecycle: hasPendingTerminal(state) ? "finalizing" : "idle",
          owner: {},
          recovery: undefined,
        };
      }
      return {
        ...state,
        lifecycle: "attaching_run",
        owner: {
          foregroundRunId: action.foregroundOwner.runId,
          ownerRevision: action.foregroundOwner.ownerRevision,
        },
        recovery: undefined,
      };
    case "owner_probe_failed":
      return enterRecovery(state, "owner_probe_failed", action.message, action.canContinueReadOnly);
    case "run_attached":
      if (
        state.owner.foregroundRunId !== action.runId
        || state.owner.ownerRevision !== action.ownerRevision
      ) {
        return enterRecovery(
          state,
          "owner_changed",
          "The foreground run owner changed before the live stream attached.",
          true,
        );
      }
      return { ...state, lifecycle: "live", recovery: undefined };
    case "refresh_started":
      return { ...state, refreshState: "loading" };
    case "refresh_failed":
      return {
        ...enterRecovery(
          state,
          "canonical_refresh_failed",
          action.message,
          action.canContinueReadOnly,
        ),
        refreshState: "failed",
      };
    case "recovery_retry_started":
      return {
        ...state,
        lifecycle: state.contractError !== undefined || !state.transcriptLoaded
          ? "loading_transcript"
          : "checking_owner",
        refreshState: hasPendingTerminal(state) ? "needed" : "idle",
        recovery: undefined,
        contractError: state.contractError,
      };
    case "continue_read_only":
      if (state.recovery?.canContinueReadOnly !== true) return state;
      return { ...state, lifecycle: "read_only" };
  }
}

export const continuityReducer = reduceConversationContinuity;

export function selectConversationTimeline(
  state: ConversationContinuityState,
): ConversationTimelineItem[] {
  const durable = [...state.canonicalItems.values()]
    .filter((item) => item.kind !== "terminal")
    .sort(compareDurableItems)
    .map((item): ConversationTimelineItem => ({
      identity: item.displayId,
      source: "durable",
      item,
    }));
  const live = [...state.liveItems.values()]
    .sort(compareLiveItems)
    .map((item): ConversationTimelineItem => ({
      identity: item.provisionalId,
      source: "live",
      item,
    }));
  return [...durable, ...live];
}

export function resolveConversationIdentity(
  state: ConversationContinuityState,
  identity: string,
): string {
  let current = identity;
  const visited = new Set<string>();
  while (!visited.has(current)) {
    visited.add(current);
    const successor = state.reconciliationSuccessors.get(current);
    if (successor === undefined) return current;
    current = successor;
  }
  return identity;
}

function receivePage(
  state: ConversationContinuityState,
  page: ConversationDisplayPage,
  mode: "initial" | "older" | "refresh",
): ConversationContinuityState {
  if (state.requestScope !== undefined && state.requestScope !== page.requestScope) {
    return rejectContract(state, {
      code: "request_scope_mismatch",
      message: "The canonical conversation page belongs to a different request scope.",
    });
  }
  if (mode === "older" && state.requestScope === undefined) {
    return rejectContract(state, {
      code: "request_scope_mismatch",
      message: "An older page cannot establish the request scope before the initial page.",
    });
  }
  if (!isDecimalSequence(page.throughSessionStreamSequence)) {
    return rejectInvalidSequence(state, "throughSessionStreamSequence");
  }
  if (!isDecimalSequence(page.totalItems)) {
    return rejectInvalidSequence(state, "totalItems");
  }
  if (page.terminalFrontier !== undefined && !isValidTerminal(page.terminalFrontier)) {
    return rejectInvalidSequence(state, "terminalFrontier");
  }
  if (page.gapFacts.some((fact) => !isDecimalSequence(fact.afterSessionStreamSequence))) {
    return rejectInvalidSequence(state, "gapFacts");
  }
  if (
    page.liveProvisionalAnchor !== undefined
    && (
      page.liveProvisionalAnchor.runId.length === 0
      || !isDecimalSequence(page.liveProvisionalAnchor.durableFrontier)
      || !isDecimalSequence(page.liveProvisionalAnchor.runSequence)
    )
  ) {
    return rejectInvalidSequence(state, "liveProvisionalAnchor");
  }
  if (page.hasMore !== (page.nextCursor !== undefined)) {
    return rejectContract(state, {
      code: "invalid_pagination",
      message: "Canonical pagination must provide a cursor exactly when more items remain.",
    });
  }

  const merged = mergeCanonicalItems(state, page.items);
  if ("error" in merged) return rejectContract(state, merged.error);

  const canonicalTerminal = mergeTerminalFrontier(
    state.canonicalTerminal,
    page.terminalFrontier,
  );
  if ("error" in canonicalTerminal) return rejectContract(state, canonicalTerminal.error);
  if (state.observedTerminal !== undefined && canonicalTerminal.value !== undefined) {
    if (
      state.observedTerminal.runId === canonicalTerminal.value.runId
      && state.observedTerminal.status !== canonicalTerminal.value.status
    ) {
      return rejectContract(state, {
        code: "terminal_conflict",
        message: `Run ${state.observedTerminal.runId} has conflicting live and durable terminal status facts.`,
      });
    }
  }

  const throughSessionStreamSequence = maxSequence(
    state.throughSessionStreamSequence,
    page.throughSessionStreamSequence,
  );
  const terminalCoveredByRefreshPage = state.observedTerminal !== undefined
    ? terminalIsCovered(
      state.observedTerminal,
      page.terminalFrontier,
      page.throughSessionStreamSequence,
    )
    : terminalRunIsCovered(
      state.pendingTerminalRunId,
      page.terminalFrontier,
      page.throughSessionStreamSequence,
    );
  const terminalSettled = mode === "refresh"
    && hasPendingTerminal(state)
    && terminalCoveredByRefreshPage;
  const lifecycle = mode === "initial"
    ? "checking_owner"
    : mode === "refresh" && state.lifecycle === "finalizing" && terminalSettled
      ? "idle"
      : state.lifecycle;
  const frontierMetadata = mode === "older"
    ? {
      totalItems: state.totalItems,
      gapFacts: mergeGapFacts(state.gapFacts, page.gapFacts),
      liveProvisionalAnchor: state.liveProvisionalAnchor,
    }
    : {
      totalItems: page.totalItems,
      gapFacts: page.gapFacts.map((fact) => ({ ...fact })),
      liveProvisionalAnchor: page.liveProvisionalAnchor === undefined
        ? undefined
        : { ...page.liveProvisionalAnchor },
    };

  return {
    ...state,
    requestScope: state.requestScope ?? page.requestScope,
    lifecycle,
    transcriptLoaded: mode === "initial" ? true : state.transcriptLoaded,
    canonicalItems: merged.canonicalItems,
    liveItems: merged.liveItems,
    reconciledIdentities: merged.reconciledIdentities,
    reconciliationSuccessors: merged.reconciliationSuccessors,
    throughSessionStreamSequence,
    totalItems: frontierMetadata.totalItems,
    nextCursor: page.nextCursor,
    hasMore: page.hasMore,
    gapFacts: frontierMetadata.gapFacts,
    liveProvisionalAnchor: frontierMetadata.liveProvisionalAnchor,
    canonicalTerminal: canonicalTerminal.value,
    observedTerminal: terminalSettled ? undefined : state.observedTerminal,
    pendingTerminalRunId: terminalSettled ? undefined : state.pendingTerminalRunId,
    refreshState: mode === "refresh"
      ? terminalSettled || !hasPendingTerminal(state) ? "idle" : "needed"
      : state.refreshState,
    recovery: mode === "refresh" ? undefined : state.recovery,
    contractError: undefined,
  };
}

function mergeCanonicalItems(
  state: ConversationContinuityState,
  incoming: readonly DurableConversationDisplayItem[],
):
  | {
    canonicalItems: ReadonlyMap<string, DurableConversationDisplayItem>;
    liveItems: ReadonlyMap<string, NormalizedLiveConversationDisplayItem>;
    reconciledIdentities: ReadonlySet<string>;
    reconciliationSuccessors: ReadonlyMap<string, string>;
  }
  | { error: ConversationContractError } {
  const canonicalItems = new Map(state.canonicalItems);
  const liveItems = new Map(state.liveItems);
  const reconciledIdentities = new Set(state.reconciledIdentities);
  const reconciliationSuccessors = new Map(state.reconciliationSuccessors);

  for (const item of incoming) {
    if (!isValidDurableItem(item)) {
      return {
        error: {
          code: "invalid_sequence",
          message: `The durable item ${item.displayId} has an invalid display order.`,
        },
      };
    }
    if (item.reconciles?.includes(item.displayId) === true) {
      return {
        error: {
          code: "invalid_reconciliation",
          message: `The durable item ${item.displayId} cannot reconcile itself.`,
        },
      };
    }

    const existing = canonicalItems.get(item.displayId);
    if (existing !== undefined) {
      if (!sameValue(existing, item)) {
        return {
          error: {
            code: "conflicting_display_id",
            message: `The durable display id ${item.displayId} was reused with different content.`,
          },
        };
      }
      continue;
    }
    if (reconciledIdentities.has(item.displayId)) continue;

    for (const identity of item.reconciles ?? []) {
      const existingSuccessor = reconciliationSuccessors.get(identity);
      if (existingSuccessor !== undefined && existingSuccessor !== item.displayId) {
        return {
          error: {
            code: "invalid_reconciliation",
            message: `The reconciled identity ${identity} has more than one durable successor.`,
          },
        };
      }
      canonicalItems.delete(identity);
      liveItems.delete(identity);
      reconciledIdentities.add(identity);
      reconciliationSuccessors.set(identity, item.displayId);
    }

    if (isFinalAssistant(item) && item.runId !== undefined) {
      const duplicateFinal = [...canonicalItems.values()].find((candidate) => (
        candidate.runId === item.runId && isFinalAssistant(candidate)
      ));
      const duplicateLiveFinal = [...liveItems.values()].find((candidate) => (
        candidate.runId === item.runId && isFinalAssistant(candidate)
      ));
      if (duplicateFinal !== undefined || duplicateLiveFinal !== undefined) {
        return {
          error: {
            code: "duplicate_final",
            message: `Run ${item.runId} has more than one unreconciled final answer.`,
          },
        };
      }
    }

    canonicalItems.set(item.displayId, item);
  }

  return { canonicalItems, liveItems, reconciledIdentities, reconciliationSuccessors };
}

function receiveLiveItem(
  state: ConversationContinuityState,
  item: LiveConversationDisplayItem,
): ConversationContinuityState {
  if (!isDecimalSequence(item.runSequence)) {
    return rejectInvalidSequence(state, `live item ${item.provisionalId}`);
  }
  if (state.reconciledIdentities.has(item.provisionalId)) return state;

  const existing = state.liveItems.get(item.provisionalId);
  const normalized = normalizeLiveItem(item);
  const normalizedItem = normalized.toolInput === undefined && existing?.toolInput !== undefined
    ? { ...normalized, toolInput: existing.toolInput }
    : normalized;
  if (existing !== undefined) {
    if (existing.runId !== normalizedItem.runId || existing.kind !== normalizedItem.kind) {
      return rejectContract(state, {
        code: "conflicting_provisional_id",
        message: `The provisional id ${item.provisionalId} changed semantic identity.`,
      });
    }
    const sequenceOrder = compareSequence(normalizedItem.runSequence, existing.runSequence);
    if (sequenceOrder < 0) return state;
    if (sequenceOrder === 0) {
      return sameValue(existing, normalizedItem)
        ? state
        : rejectContract(state, {
          code: "conflicting_provisional_id",
          message: `The provisional id ${item.provisionalId} conflicts at the same run sequence.`,
        });
    }
    if (statusRegresses(existing.status, normalizedItem.status)) {
      return rejectContract(state, {
        code: "conflicting_provisional_id",
        message: `The provisional id ${item.provisionalId} regressed its status.`,
      });
    }
  }

  if (isFinalAssistant(normalizedItem)) {
    const duplicateDurable = [...state.canonicalItems.values()].find((candidate) => (
      candidate.runId === normalizedItem.runId && isFinalAssistant(candidate)
    ));
    const duplicateLive = [...state.liveItems.values()].find((candidate) => (
      candidate.runId === normalizedItem.runId
      && candidate.provisionalId !== normalizedItem.provisionalId
      && isFinalAssistant(candidate)
    ));
    if (duplicateDurable !== undefined || duplicateLive !== undefined) {
      return rejectContract(state, {
        code: "duplicate_final",
        message: `Run ${normalizedItem.runId} has more than one unreconciled final answer.`,
      });
    }
  }

  const liveItems = new Map(state.liveItems);
  liveItems.set(normalizedItem.provisionalId, normalizedItem);
  return { ...state, liveItems };
}

function normalizeLiveItem(
  item: LiveConversationDisplayItem,
): NormalizedLiveConversationDisplayItem {
  return { ...item, content: normalizeLiveContent(item.content) };
}

function normalizeLiveContent(
  content: LiveConversationDisplayContent,
): Exclude<SharedConversationDisplayContent, { type: "terminal" }> {
  switch (content.type) {
    case "message": {
      const text = content.text ?? "";
      return {
        ...content,
        imageAttachmentCount: content.imageAttachmentCount ?? 0,
        truncated: content.truncated ?? false,
        originalContentBytes: content.originalContentBytes ?? utf8Length(text),
      };
    }
    case "reasoning":
    case "notice":
      return {
        ...content,
        truncated: content.truncated ?? false,
        originalContentBytes: content.originalContentBytes ?? utf8Length(content.text),
      };
    case "tool": {
      const output = content.output ?? "";
      return {
        ...content,
        truncated: content.truncated ?? false,
        originalContentBytes: content.originalContentBytes ?? utf8Length(output),
      };
    }
    case "approval":
    case "checkpoint":
      return content;
  }
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).length;
}

function observeTerminal(
  state: ConversationContinuityState,
  terminal: ConversationTerminalObservation,
): ConversationContinuityState {
  if (!isValidTerminalObservation(terminal)) {
    return rejectContract(state, {
      code: "terminal_conflict",
      message: "The observed terminal run identity is invalid.",
    });
  }
  if (state.observedTerminal !== undefined) {
    if (sameValue(state.observedTerminal, terminal)) return state;
    return rejectContract(state, {
      code: "terminal_conflict",
      message: "A second terminal transport fact conflicts with the run being finalized.",
    });
  }
  if (
    state.pendingTerminalRunId !== undefined
    && state.pendingTerminalRunId !== terminal.runId
  ) {
    return rejectContract(state, {
      code: "terminal_conflict",
      message: "A terminal transport fact conflicts with the run being finalized.",
    });
  }
  if (
    state.canonicalTerminal?.runId === terminal.runId
    && state.canonicalTerminal.status !== terminal.status
  ) {
    return rejectContract(state, {
      code: "terminal_conflict",
      message: `Run ${terminal.runId} has conflicting live and durable terminal status facts.`,
    });
  }
  if (terminalIsCovered(terminal, state.canonicalTerminal, state.throughSessionStreamSequence)) {
    return state;
  }
  return {
    ...state,
    lifecycle: "finalizing",
    observedTerminal: { ...terminal },
    pendingTerminalRunId: undefined,
    refreshState: "needed",
    recovery: undefined,
  };
}

function observeTerminalTransport(
  state: ConversationContinuityState,
  runId: string,
): ConversationContinuityState {
  if (runId.length === 0) {
    return rejectContract(state, {
      code: "terminal_conflict",
      message: "The terminal transport run identity is invalid.",
    });
  }
  if (state.observedTerminal !== undefined) {
    return state.observedTerminal.runId === runId
      ? state
      : rejectContract(state, {
        code: "terminal_conflict",
        message: "A terminal transport fact conflicts with the run being finalized.",
      });
  }
  if (state.pendingTerminalRunId !== undefined) {
    return state.pendingTerminalRunId === runId
      ? state
      : rejectContract(state, {
        code: "terminal_conflict",
        message: "A second terminal transport fact conflicts with the run being finalized.",
      });
  }
  return {
    ...state,
    lifecycle: "finalizing",
    pendingTerminalRunId: runId,
    refreshState: "needed",
    recovery: undefined,
  };
}

function mergeTerminalFrontier(
  current: ConversationTerminalFrontier | undefined,
  incoming: ConversationTerminalFrontier | undefined,
): { value: ConversationTerminalFrontier | undefined } | { error: ConversationContractError } {
  if (incoming === undefined) return { value: current };
  if (current === undefined) return { value: incoming };

  if (current.runId === incoming.runId && current.status !== incoming.status) {
    return {
      error: {
        code: "terminal_conflict",
        message: `Run ${current.runId} was replayed with conflicting terminal status facts.`,
      },
    };
  }

  const order = compareSequence(incoming.sessionStreamSequence, current.sessionStreamSequence);
  if (order < 0) return { value: current };
  if (order > 0) return { value: incoming };
  if (current.runId === incoming.runId && current.status === incoming.status) {
    return { value: current };
  }
  return {
    error: {
      code: "terminal_conflict",
      message: "The same terminal frontier was replayed with conflicting run or status facts.",
    },
  };
}

function terminalIsCovered(
  observed: ConversationTerminalObservation | undefined,
  canonical: ConversationTerminalFrontier | undefined,
  through: DecimalSequence | undefined,
): boolean {
  if (observed === undefined) return true;
  if (canonical === undefined || through === undefined) return false;
  if (canonical.runId !== observed.runId || canonical.status !== observed.status) return false;
  return compareSequence(through, canonical.sessionStreamSequence) >= 0;
}

function terminalRunIsCovered(
  runId: string | undefined,
  canonical: ConversationTerminalFrontier | undefined,
  through: DecimalSequence | undefined,
): boolean {
  if (runId === undefined) return true;
  if (canonical === undefined || through === undefined || canonical.runId !== runId) return false;
  return compareSequence(through, canonical.sessionStreamSequence) >= 0;
}

function hasPendingTerminal(state: ConversationContinuityState): boolean {
  return state.observedTerminal !== undefined || state.pendingTerminalRunId !== undefined;
}

function enterRecovery(
  state: ConversationContinuityState,
  code: string,
  message: string,
  canContinueReadOnly: boolean,
): ConversationContinuityState {
  return {
    ...state,
    lifecycle: "read_only_recovery",
    recovery: { code, message, canContinueReadOnly },
  };
}

function rejectInvalidSequence(
  state: ConversationContinuityState,
  field: string,
): ConversationContinuityState {
  return rejectContract(state, {
    code: "invalid_sequence",
    message: `${field} is not an unsigned decimal sequence.`,
  });
}

function rejectContract(
  state: ConversationContinuityState,
  error: ConversationContractError,
): ConversationContinuityState {
  return {
    ...state,
    lifecycle: "error",
    refreshState: "failed",
    contractError: error,
    recovery: {
      code: "contract_violation",
      message: error.message,
      canContinueReadOnly: state.transcriptLoaded,
    },
  };
}

function compareDurableItems(
  left: DurableConversationDisplayItem,
  right: DurableConversationDisplayItem,
): number {
  const sequence = compareSequence(
    left.displayOrder.sessionStreamSequence,
    right.displayOrder.sessionStreamSequence,
  );
  if (sequence !== 0) return sequence;
  if (left.displayOrder.subindex !== right.displayOrder.subindex) {
    return left.displayOrder.subindex - right.displayOrder.subindex;
  }
  return left.displayId.localeCompare(right.displayId);
}

function compareLiveItems(
  left: LiveConversationDisplayItem,
  right: LiveConversationDisplayItem,
): number {
  const run = left.runId.localeCompare(right.runId);
  if (run !== 0) return run;
  const sequence = compareSequence(left.runSequence, right.runSequence);
  return sequence !== 0 ? sequence : left.provisionalId.localeCompare(right.provisionalId);
}

function compareSequence(left: DecimalSequence, right: DecimalSequence): number {
  const leftValue = BigInt(left);
  const rightValue = BigInt(right);
  return leftValue < rightValue ? -1 : leftValue > rightValue ? 1 : 0;
}

function maxSequence(
  left: DecimalSequence | undefined,
  right: DecimalSequence,
): DecimalSequence {
  return left === undefined || compareSequence(left, right) < 0 ? right : left;
}

function mergeGapFacts(
  current: readonly ConversationDisplayGapFact[],
  incoming: readonly ConversationDisplayGapFact[],
): ConversationDisplayGapFact[] {
  const merged = new Map<string, ConversationDisplayGapFact>();
  for (const fact of [...current, ...incoming]) {
    merged.set(`${fact.kind}:${fact.afterSessionStreamSequence}`, { ...fact });
  }
  return [...merged.values()].sort((left, right) => {
    const order = compareSequence(
      left.afterSessionStreamSequence,
      right.afterSessionStreamSequence,
    );
    return order !== 0 ? order : left.kind.localeCompare(right.kind);
  });
}

function isDecimalSequence(value: string): boolean {
  return /^(0|[1-9][0-9]*)$/.test(value);
}

function isValidDurableItem(item: DurableConversationDisplayItem): boolean {
  return item.displayId.length > 0
    && item.sourceEventId.length > 0
    && isDecimalSequence(item.displayOrder.sessionStreamSequence)
    && Number.isSafeInteger(item.displayOrder.subindex)
    && item.displayOrder.subindex >= 0
    && (item.runSequence === undefined || isDecimalSequence(item.runSequence));
}

function isValidTerminal(terminal: ConversationTerminalFrontier): boolean {
  return terminal.runId.length > 0 && isDecimalSequence(terminal.sessionStreamSequence);
}

function isValidTerminalObservation(terminal: ConversationTerminalObservation): boolean {
  return terminal.runId.length > 0;
}

function isFinalAssistant(
  item: DurableConversationDisplayItem | LiveConversationDisplayItem,
): boolean {
  return item.kind === "assistant_message"
    && item.content.type === "message"
    && item.content.role === "assistant"
    && item.content.assistantPhase === "final_answer";
}

function statusRegresses(
  current: ConversationDisplayStatus,
  incoming: ConversationDisplayStatus,
): boolean {
  const currentRank = statusRank(current);
  const incomingRank = statusRank(incoming);
  if (incomingRank < currentRank) return true;
  return currentRank === 4 && incomingRank === 4 && current !== incoming;
}

function statusRank(status: ConversationDisplayStatus): number {
  switch (status) {
    case "recorded":
    case "requested":
      return 1;
    case "running":
    case "streaming":
    case "waiting_for_approval":
      return 2;
    case "approved":
      return 3;
    case "denied":
    case "completed":
    case "succeeded":
    case "failed":
    case "cancelled":
    case "interrupted":
    case "blocked":
      return 4;
  }
}

function sameValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left)
      && Array.isArray(right)
      && left.length === right.length
      && left.every((value, index) => sameValue(value, right[index]));
  }
  if (left === null || right === null || typeof left !== "object" || typeof right !== "object") {
    return false;
  }
  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  const leftKeys = Object.keys(leftRecord).filter((key) => leftRecord[key] !== undefined).sort();
  const rightKeys = Object.keys(rightRecord).filter((key) => rightRecord[key] !== undefined).sort();
  return leftKeys.length === rightKeys.length
    && leftKeys.every((key, index) => (
      key === rightKeys[index] && sameValue(leftRecord[key], rightRecord[key])
    ));
}
