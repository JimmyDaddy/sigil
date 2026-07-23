import { useState } from "react";

import { useLocale } from "./i18n";
import type {
  ConversationQueueCommandAction,
  ConversationQueueItem,
  ConversationQueueView,
  ReasoningEffort,
} from "./types";
import { Icon } from "./ui/icons";
import { Button, IconButton, TextArea, Tooltip } from "./ui/primitives";

export function ConversationQueuePanel({
  queue,
  busy,
  error,
  reasoningEffort,
  onRefresh,
  onCommand,
}: {
  readonly queue?: ConversationQueueView;
  readonly busy: boolean;
  readonly error: boolean;
  readonly reasoningEffort?: ReasoningEffort;
  readonly onRefresh: () => void;
  readonly onCommand: (action: ConversationQueueCommandAction) => Promise<boolean>;
}) {
  const { t } = useLocale();
  const [editingEntryId, setEditingEntryId] = useState<string>();
  const [replacementPrompt, setReplacementPrompt] = useState("");

  const beginEdit = (item: ConversationQueueItem) => {
    setEditingEntryId(item.entryId);
    setReplacementPrompt("");
  };
  const finishEdit = async (entryId: string) => {
    const prompt = replacementPrompt.trim();
    if (prompt === "") return;
    if (await onCommand({
      action: "edit",
      entryId,
      prompt,
      reasoningEffort,
    })) {
      setEditingEntryId(undefined);
      setReplacementPrompt("");
    }
  };

  if (queue === undefined) {
    return (
      <div className="conversation-queue-empty">
        <p>{error ? t("conversationQueueUnavailable") : t("loadingConversationQueue")}</p>
        {error ? <Button type="button" onClick={onRefresh}>{t("retry")}</Button> : null}
      </div>
    );
  }

  return (
    <div className="conversation-queue-panel sg-bounded-content">
      <header className="conversation-queue-toolbar">
        <div>
          <strong>{t("queuedMessages", { count: queue.totalItems })}</strong>
          <small>{queue.paused ? t("conversationQueuePaused") : t("conversationQueueAutomatic")}</small>
        </div>
        <Tooltip label={queue.paused ? t("resumeQueue") : t("pauseQueue")}>
          <IconButton
            type="button"
            aria-label={queue.paused ? t("resumeQueue") : t("pauseQueue")}
            aria-busy={busy || undefined}
            icon={<Icon name={queue.paused ? "play" : "pause"} />}
            disabled={busy}
            onClick={() => void onCommand({ action: queue.paused ? "resume" : "pause" })}
          />
        </Tooltip>
      </header>

      {queue.items.length === 0 ? (
        <div className="conversation-queue-empty">
          <Icon name="queue" />
          <strong>{t("conversationQueueEmpty")}</strong>
          <p>{t("conversationQueueEmptyDetail")}</p>
        </div>
      ) : (
        <ol className="conversation-queue-list">
          {queue.items.map((item, index) => {
            const editing = editingEntryId === item.entryId;
            const requiresReentry = item.promptMaterial === "requires_reentry";
            return (
              <li
                className={`conversation-queue-item status-${item.status}${requiresReentry ? " requires-reentry" : ""}`}
                key={item.entryId}
              >
                <header>
                  <span className="conversation-queue-order" aria-label={t("queuePosition", { position: index + 1 })}>
                    {index + 1}
                  </span>
                  <span className="conversation-queue-summary">
                    <strong>{item.promptPreview}</strong>
                    <small>{queueItemState(item, t)}</small>
                  </span>
                  <span className="conversation-queue-actions">
                    <Tooltip label={requiresReentry ? t("reenterQueuedMessage") : t("replaceQueuedMessage")}>
                      <IconButton
                        type="button"
                        aria-label={requiresReentry ? t("reenterQueuedMessage") : t("replaceQueuedMessage")}
                        icon={<Icon name="edit" />}
                        disabled={busy || item.status !== "queued"}
                        onClick={() => beginEdit(item)}
                      />
                    </Tooltip>
                    <Tooltip label={t("moveQueuedMessageUp")}>
                      <IconButton
                        type="button"
                        aria-label={t("moveQueuedMessageUp")}
                        icon={<Icon name="chevron-up" />}
                        disabled={busy || index === 0 || item.status !== "queued"}
                        onClick={() => void onCommand({
                          action: "reorder",
                          entryId: item.entryId,
                          afterEntryId: index < 2 ? undefined : queue.items[index - 2]?.entryId,
                        })}
                      />
                    </Tooltip>
                    <Tooltip label={t("moveQueuedMessageDown")}>
                      <IconButton
                        type="button"
                        aria-label={t("moveQueuedMessageDown")}
                        icon={<Icon name="chevron-down" />}
                        disabled={busy || index >= queue.items.length - 1 || item.status !== "queued"}
                        onClick={() => void onCommand({
                          action: "reorder",
                          entryId: item.entryId,
                          afterEntryId: queue.items[index + 1]?.entryId,
                        })}
                      />
                    </Tooltip>
                    <Tooltip label={t("removeQueuedMessage")}>
                      <IconButton
                        type="button"
                        aria-label={t("removeQueuedMessage")}
                        icon={<Icon name="delete" />}
                        disabled={busy || item.status !== "queued"}
                        onClick={() => void onCommand({ action: "remove", entryId: item.entryId })}
                      />
                    </Tooltip>
                  </span>
                </header>
                {editing ? (
                  <form
                    className="conversation-queue-edit"
                    onSubmit={(event) => {
                      event.preventDefault();
                      void finishEdit(item.entryId);
                    }}
                  >
                    <TextArea
                      label={requiresReentry ? t("reenterQueuedMessage") : t("replacementPrompt")}
                      value={replacementPrompt}
                      placeholder={t("replacementPromptPlaceholder")}
                      disabled={busy}
                      autoFocus
                      rows={2}
                      onChange={(event) => setReplacementPrompt(event.target.value)}
                    />
                    <div className="conversation-queue-edit-actions">
                      <Button type="button" variant="quiet" disabled={busy} onClick={() => {
                        setEditingEntryId(undefined);
                        setReplacementPrompt("");
                      }}>
                        {t("cancel")}
                      </Button>
                      <Button type="submit" variant="primary" busy={busy} disabled={replacementPrompt.trim() === ""}>
                        {requiresReentry ? t("saveAndRestoreQueueItem") : t("replace")}
                      </Button>
                    </div>
                  </form>
                ) : null}
              </li>
            );
          })}
        </ol>
      )}
      {queue.truncated ? <p className="conversation-queue-truncated">{t("conversationQueueTruncated")}</p> : null}
    </div>
  );
}

function queueItemState(
  item: ConversationQueueItem,
  t: ReturnType<typeof useLocale>["t"],
): string {
  if (item.promptMaterial === "requires_reentry") return t("queuedMessageNeedsReentry");
  if (item.blockedReason === "queue_paused") return t("queuedMessagePaused");
  if (item.blockedReason === "foreground_run_active") return t("queuedMessageWaitingForRun");
  if (item.blockedReason === "waiting_for_terminal_frontier") return t("queuedMessageWaitingForTerminal");
  switch (item.status) {
    case "queued": return item.dispatchable ? t("queuedMessageReady") : t("queuedMessageWaiting");
    case "dispatching": return t("queuedMessageDispatching");
    case "delivered": return t("queuedMessageDelivered");
    case "rejected": return t("queuedMessageRejected");
    case "cancelled": return t("queuedMessageCancelled");
    case "stale": return t("queuedMessageStale");
    case "unknown": return t("queuedMessageUnknown");
  }
}
