import type { Translate } from "../../i18n";
import type { ProviderModelRef, TimelineProviderTurnRecovery } from "../../types";

/** Navigation context only; it never authorizes retries or credential mutations. */
export interface ProviderRecoverySettingsContext {
  readonly runId: string;
  readonly reasonCode?: string;
  readonly failedModelRef?: ProviderModelRef;
}

export function providerRecoveryPresentation(recovery: TimelineProviderTurnRecovery, t: Translate) {
  if (recovery.reasonCode === "provider_recovery_cancelled" && recovery.phase === "paused") {
    return { neutral: true, title: t("providerRecoveryStopping"), detail: t("providerRecoveryStoppingDetail") };
  }
  if (recovery.reasonCode === "provider_authentication_required") {
    return { neutral: false, title: t("providerRecoveryAuthentication"), detail: t("providerRecoveryAuthenticationDetail") };
  }
  if (recovery.reasonCode === "effect_reconciliation_required") {
    return { neutral: false, title: t("providerRecoveryEffect"), detail: t("providerRecoveryEffectDetail") };
  }
  const title = t(recovery.phase === "waiting" ? "providerRecoveryWaiting"
    : recovery.phase === "recovering" ? "providerRecoveryRecovering"
    : recovery.phase === "paused" ? "providerRecoveryPaused" : "providerRecoveryBlocked");
  return { neutral: !recovery.userAttentionRequired, title,
    detail: recovery.userAttentionRequired ? t("providerRecoveryNeedsAttention")
      : t("providerRecoveryAttempt", { current: String(recovery.activeRetryCount), total: String(recovery.activeMaxRetries) }) };
}
