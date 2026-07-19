import { useState, type RefObject } from "react";

const MAX_DRAFT_BYTES = 256 * 1024;

export function Composer({
  draftKey,
  active,
  submitting,
  controlBusy,
  composerRef,
  onSubmit,
  onCancel,
}: {
  draftKey: string;
  active: boolean;
  submitting: boolean;
  controlBusy: boolean;
  composerRef: RefObject<HTMLTextAreaElement | null>;
  onSubmit: (prompt: string) => Promise<boolean>;
  onCancel: () => void;
}) {
  const [prompt, setPrompt] = useState(() => readDraft(draftKey));
  const submit = async () => {
    const nextPrompt = prompt.trim();
    if (nextPrompt === "" || active || submitting) return;
    if (await onSubmit(nextPrompt)) {
      setPrompt("");
      writeDraft(draftKey, "");
    }
  };
  return (
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
      <label htmlFor="desktop-prompt">Message Sigil</label>
      <textarea
        id="desktop-prompt"
        ref={composerRef}
        value={prompt}
        onChange={(event) => {
          setPrompt(event.target.value);
          writeDraft(draftKey, event.target.value);
        }}
        placeholder="Describe the change or question…"
        rows={4}
        aria-describedby="composer-hint"
        onKeyDown={(event) => {
          if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
          event.preventDefault();
          void submit();
        }}
      />
      <div className="composer-actions">
        <small id="composer-hint">{active ? "Draft saved on this device. Send it after the active run finishes." : "Enter to send · Shift+Enter for a new line · Drafts stay on this device"}</small>
        <div>
          {active ? <button className="quiet-button danger-button" type="button" disabled={controlBusy} onClick={onCancel}>Cancel run</button> : null}
          <button className="primary-button" type="submit" disabled={prompt.trim() === "" || active || submitting}>{submitting ? "Starting…" : "Run"}</button>
        </div>
      </div>
    </form>
  );
}

export function draftStorageKey(workspaceId: string, sessionId: string): string {
  return `sigil:conversation-draft:v1:${workspaceId}:${sessionId}`;
}

function readDraft(key: string): string {
  try {
    return window.localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function writeDraft(key: string, value: string): void {
  try {
    if (new TextEncoder().encode(value).byteLength > MAX_DRAFT_BYTES) {
      window.localStorage.removeItem(key);
      return;
    }
    if (value === "") window.localStorage.removeItem(key);
    else window.localStorage.setItem(key, value);
  } catch {
    // Draft persistence is best-effort; the controlled input remains usable.
  }
}
