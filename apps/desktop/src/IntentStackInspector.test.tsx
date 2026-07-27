import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { IntentStackInspector } from "./IntentStackInspector";
import { LocaleProvider } from "./i18n";
import type {
  IntentDropBinding,
  IntentDropPreview,
  IntentStackState,
  IntentVersionRef,
} from "./types";

afterEach(cleanup);

const firstIntentRef: IntentVersionRef = { intentId: "intent-core", version: 2 };
const secondIntentRef: IntentVersionRef = { intentId: "intent-docs", version: 1 };

const state: IntentStackState = {
  status: "available",
  schemaVersion: 1,
  stack: {
    schemaVersion: 1,
    stackId: "stack-main",
    stackVersion: 7,
    authorityState: "active",
    planDigest: `sha256:jcs-v1:${"a".repeat(64)}`,
    conflicts: [],
    intents: [
      {
        intentRef: firstIntentRef,
        title: "Implement exact drop",
        statement: "Keep the command bound to a fresh server preview.",
        acceptanceCriteria: [
          {
            criterionId: "criterion-exact-binding",
            statement: "The renderer emits only the exact operation binding.",
            required: true,
          },
        ],
        dependsOn: [],
        source: { kind: "user_turn", sourceTurnId: "turn-17" },
        definitionState: "accepted",
        applicationState: "applied",
        exclusiveArtifactCount: 1,
        sharedArtifactCount: 0,
        unownedArtifactCount: 0,
        driftedArtifactCount: 0,
        unavailableArtifactCount: 0,
        advisoryCriterionCount: 0,
        systemVerifiedCriterionCount: 1,
        artifacts: [
          {
            artifactId: "artifact-client",
            artifactKind: "file_hunk",
            ownership: "exclusive",
            availability: "available",
            normalizedRelativePath: "apps/desktop/src/bridge.ts",
          },
        ],
        availableActions: ["drop"],
      },
      {
        intentRef: secondIntentRef,
        title: "Document the flow",
        statement: "Keep the durable user flow documented.",
        acceptanceCriteria: [],
        dependsOn: ["intent-core"],
        source: { kind: "trusted_spec", safeSourceLabel: "RFC-0051" },
        definitionState: "accepted",
        applicationState: "unapplied",
        exclusiveArtifactCount: 0,
        sharedArtifactCount: 0,
        unownedArtifactCount: 0,
        driftedArtifactCount: 0,
        unavailableArtifactCount: 0,
        advisoryCriterionCount: 0,
        systemVerifiedCriterionCount: 0,
        artifacts: [],
        availableActions: [],
      },
    ],
  },
};

const preview: IntentDropPreview = {
  schemaVersion: 1,
  operationId: "operation-drop-core",
  operationKind: "drop",
  stackId: "stack-main",
  stackVersion: 7,
  targetIntents: [firstIntentRef],
  targetIsLeaf: true,
  workspaceRevision: 19,
  fileEffects: [
    {
      normalizedRelativePath: "apps/desktop/src/bridge.ts",
      action: "update",
      artifactIds: ["artifact-client"],
    },
  ],
  retainedIntents: [secondIntentRef],
  verificationImpacts: [{ receiptId: "receipt-desktop", impact: "rerun_required" }],
  conflicts: [],
  previewDigest: `sha256:jcs-v1:${"b".repeat(64)}`,
};

function renderInspector({
  dropPreview,
  onPreview = vi.fn((_intentRef: IntentVersionRef) => undefined),
  onConfirm = vi.fn((_request: IntentDropBinding) => undefined),
}: {
  dropPreview?: IntentDropPreview;
  onPreview?: (intentRef: IntentVersionRef) => void;
  onConfirm?: (request: IntentDropBinding) => void;
} = {}) {
  render(
    <LocaleProvider>
      <IntentStackInspector
        state={state}
        preview={dropPreview}
        loading={false}
        busy={false}
        error={false}
        runActive={false}
        onPreview={onPreview}
        onConfirm={onConfirm}
        onRefresh={() => undefined}
      />
    </LocaleProvider>,
  );
  return { onPreview, onConfirm };
}

describe("Intent Stack inspector", () => {
  it("keeps list selection and bounded intent details in one review surface", async () => {
    const user = userEvent.setup();
    renderInspector();

    expect(screen.getByRole("heading", { name: "Implement exact drop" })).toBeTruthy();
    expect(screen.getByText("apps/desktop/src/bridge.ts")).toBeTruthy();
    expect(screen.getByText("Accepted from user turn turn-17")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: /Document the flow/ }));

    expect(screen.getByRole("heading", { name: "Document the flow" })).toBeTruthy();
    expect(screen.getByText("Accepted trusted specification: RFC-0051")).toBeTruthy();
    expect(screen.getByText("No bounded artifacts are attributed to this intent.")).toBeTruthy();
  });

  it("requests a preview for the exact selected intent version", async () => {
    const user = userEvent.setup();
    const { onPreview } = renderInspector();

    await user.click(screen.getByRole("button", { name: "Preview exact drop" }));

    expect(onPreview).toHaveBeenCalledWith(firstIntentRef);
  });

  it("confirms only the stale-safe operation binding", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn((_request: IntentDropBinding) => undefined);
    renderInspector({ dropPreview: preview, onConfirm });

    await user.click(screen.getByRole("button", { name: "Confirm drop of “Implement exact drop”" }));

    expect(onConfirm).toHaveBeenCalledWith({
      operationId: preview.operationId,
      stackVersion: preview.stackVersion,
      previewDigest: preview.previewDigest,
    });
    const serialized = JSON.stringify(onConfirm.mock.calls[0]?.[0]);
    expect(serialized).not.toContain("normalizedRelativePath");
    expect(serialized).not.toContain("authority");
    expect(serialized).not.toContain("policy");
  });

  it("fails closed when the exact preview contains a conflict", () => {
    renderInspector({
      dropPreview: {
        ...preview,
        targetIsLeaf: false,
        conflicts: [
          {
            code: "target_not_leaf",
            intentRef: firstIntentRef,
            safeReason: "A dependent intent must be removed first.",
          },
        ],
      },
    });

    expect(screen.getByRole("alert").textContent).toContain("A dependent intent must be removed first.");
    expect((screen.getByRole("button", {
      name: "Confirm drop of “Implement exact drop”",
    }) as HTMLButtonElement).disabled).toBe(true);
  });
});
