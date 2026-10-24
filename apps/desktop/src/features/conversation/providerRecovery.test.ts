import { expect, it } from "vitest";

import { translateEnglish } from "../../i18n";
import type { TimelineProviderTurnRecovery } from "../../types";
import { providerRecoveryPresentation } from "./providerRecovery";

const recovery: TimelineProviderTurnRecovery = { phase: "blocked", activeRetryCount: 0, activeMaxRetries: 2,
  retryCount: 0, maxTransportRetries: 2, availableActions: ["update_connection"], userAttentionRequired: true };

it("explains typed authentication without assuming an expired or user-entered API key", () => {
  const copy = providerRecoveryPresentation({ ...recovery, reasonCode: "provider_authentication_required" }, translateEnglish);
  expect(copy.title).toBe("Connection authentication failed");
  expect(copy.neutral).toBe(false);
  expect(copy.detail).toContain("authentication method and access permissions");
  expect(copy.detail).not.toMatch(/expired|API key|401/);
});

it("uses neutral cancellation feedback while preserving real blockers with the same paused phase", () => {
  expect(providerRecoveryPresentation({ ...recovery, phase: "paused", reasonCode: "provider_recovery_cancelled" }, translateEnglish))
    .toMatchObject({ neutral: true, title: "Stopping the current request" });
  expect(providerRecoveryPresentation({ ...recovery, phase: "paused", reasonCode: "effect_reconciliation_required" }, translateEnglish))
    .toMatchObject({ neutral: false, title: "Confirm the external operation result" });
});

it("does not infer authentication from unknown reason strings or treat a blocked turn as cancellation", () => {
  for (const reasonCode of ["401 authentication failed", "provider_configuration_or_capacity_required", "provider_recovery_cancelled"]) {
    expect(providerRecoveryPresentation({ ...recovery, reasonCode }, translateEnglish))
      .toMatchObject({ neutral: false, title: "Model recovery needs attention" });
  }
});
