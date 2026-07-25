import type { RunStatus, RunStreamState } from "../../types";
import type { ConversationContinuityState } from "./continuityReducer";

export type ComposerActivityState =
  | "starting"
  | "connecting"
  | "running"
  | "waiting_for_approval"
  | "stopping"
  | "recovering"
  | "reconnecting"
  | "connection_error"
  | "finalizing";

export function resolveComposerActivityState({
  active,
  submitting,
  controlBusy,
  approvalPending,
  runStatus,
  streamState,
  continuityLifecycle,
}: {
  readonly active: boolean;
  readonly submitting: boolean;
  readonly controlBusy: boolean;
  readonly approvalPending: boolean;
  readonly runStatus?: RunStatus;
  readonly streamState?: RunStreamState;
  readonly continuityLifecycle: ConversationContinuityState["lifecycle"];
}): ComposerActivityState | undefined {
  if (approvalPending || runStatus === "waiting_for_approval") return "waiting_for_approval";
  if (runStatus === "cancel_requested" || (active && controlBusy)) return "stopping";
  if (submitting || runStatus === "starting") return "starting";
  if (continuityLifecycle === "finalizing") return "finalizing";
  if (
    runStatus === "execution_uncertain"
    || continuityLifecycle === "checking_owner"
    || continuityLifecycle === "attaching_run"
  ) return "recovering";
  if (streamState === "error") return "connection_error";
  if (streamState === "reconnecting") return "reconnecting";
  if (streamState === "connecting") return "connecting";
  if (active) return "running";
  return undefined;
}
