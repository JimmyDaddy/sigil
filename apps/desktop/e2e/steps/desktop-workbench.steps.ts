import assert from "node:assert/strict";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { resolve } from "node:path";

import { After, Given, Then, When } from "@wdio/cucumber-framework";
import { $, $$, browser } from "@wdio/globals";

import { desktopProviderCanaries } from "../provider-fixture";

const unavailableSessionRef = "desktop-e2e-unsupported.jsonl";
let unavailableSessionPath: string | undefined;
interface DurableUserInputIdentity {
  readonly requestId: string;
  readonly generation: number;
  readonly requestHash: string;
}
let durableUserInputIdentity: DurableUserInputIdentity | undefined;
let planClarificationIdentity: DurableUserInputIdentity | undefined;

After(async ({ result, pickle }) => {
  if (result?.status !== "FAILED") return;
  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  if (artifactRoot === undefined) return;
  const directory = resolve(artifactRoot, `failure-${Date.now()}`);
  mkdirSync(directory, { recursive: true });
  const ui = await browser.execute(() => ({
    body: document.body.textContent?.slice(0, 24_000),
    panels: [...document.querySelectorAll<HTMLElement>(".conversation-panel")]
      .map((panel) => ({ ...panel.dataset })),
    messages: [...document.querySelectorAll<HTMLElement>(".timeline .message-assistant, .timeline .message-progress")]
      .map((row) => ({
        classes: row.className,
        length: row.textContent?.length,
        head: row.textContent?.slice(0, 200),
        tail: row.textContent?.slice(-200),
        contentClasses: [...row.querySelectorAll<HTMLElement>("[class]")]
          .map((node) => node.className).slice(0, 20),
      })),
    planActions: [...document.querySelectorAll<HTMLButtonElement>("[data-plan-action]")]
      .map((button) => ({ ...button.dataset, disabled: button.disabled })),
  }));
  const evidence = await providerEvidence().catch(() => undefined);
  writeFileSync(resolve(directory, "snapshot.json"), JSON.stringify({ scenario: pickle.name, ui, evidence }, null, 2));
  await browser.saveScreenshot(resolve(directory, "desktop.png"));
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  if (runtimeRoot !== undefined) {
    const sessionRoot = resolve(runtimeRoot, "state", "managed", "session-log");
    if (existsSync(sessionRoot)) cpSync(sessionRoot, resolve(directory, "session-logs"), { recursive: true });
    const controlRoot = resolve(runtimeRoot, "state", "managed", "application-control-log");
    if (existsSync(controlRoot)) cpSync(controlRoot, resolve(directory, "application-control-logs"), { recursive: true });
  }
});

async function waitForComposerEnabled(
  timeoutMsg = "the desktop composer did not become enabled",
  timeout = 20_000,
): Promise<void> {
  await browser.waitUntil(
    async () => await browser.execute(() => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const input = panel?.querySelector<HTMLTextAreaElement>("#desktop-prompt");
      const lifecycle = panel?.dataset.continuityLifecycle;
      return input !== null && input !== undefined
        && !input.disabled
        && (lifecycle === "idle" || lifecycle === "live");
    }),
    { timeout, timeoutMsg },
  );
}

async function setComposerPrompt(value: string): Promise<void> {
  await browser.execute((nextValue) => {
    const panel = document.querySelector<HTMLElement>(".conversation-panel");
    const input = panel?.querySelector<HTMLTextAreaElement>("#desktop-prompt");
    if (input === null || input === undefined) throw new Error("the desktop composer disappeared before input");
    input.focus({ preventScroll: true });
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    if (setter === undefined) throw new Error("the desktop composer value setter is unavailable");
    setter.call(input, nextValue);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }, value);
  try {
    await browser.waitUntil(
      async () => await browser.execute(
        (nextValue) => {
          const panel = document.querySelector<HTMLElement>(".conversation-panel");
          const input = panel?.querySelector<HTMLTextAreaElement>("#desktop-prompt");
          const submit = panel?.querySelector<HTMLButtonElement>(
            ".composer-submit:not(.composer-stop)",
          );
          return input?.value === nextValue
            && input.disabled === false
            && submit?.disabled === false;
        },
        value,
      ),
      {
        timeout: 5_000,
        timeoutMsg: "the desktop composer did not retain the prompt or enable send",
      },
    );
  } catch (error) {
    const state = await browser.execute(() => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const input = panel?.querySelector<HTMLTextAreaElement>("#desktop-prompt");
      const submit = panel?.querySelector<HTMLButtonElement>(
        ".composer-submit:not(.composer-stop)",
      );
      return {
        sessionId: panel?.dataset.sessionId,
        lifecycle: panel?.dataset.continuityLifecycle,
        value: input?.value,
        inputDisabled: input?.disabled,
        submitDisabled: submit?.disabled,
      };
    });
    throw new Error(`the desktop composer did not retain the prompt or enable send: ${JSON.stringify(state)}`, {
      cause: error,
    });
  }
}

async function submitComposerPrompt(): Promise<void> {
  const panel = await $(".conversation-panel");
  const submit = await panel.$(".composer-submit:not(.composer-stop)");
  await submit.waitForEnabled({
    timeout: 5_000,
    timeoutMsg: "the desktop composer did not expose an enabled send action",
  });
  await submit.click();
}

Given("the current-source desktop has restored the isolated workspace", async () => {
  await $(".app-shell").waitForDisplayed();
  const workspace = await $(".workspace-switcher");
  try {
    await workspace.waitUntil(
      async () => (await workspace.getText()).includes("desktop-e2e-workspace"),
      { timeout: 20_000, timeoutMsg: "the isolated workspace was not restored" },
    );
  } catch (error) {
    const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
    if (artifactRoot !== undefined) {
      await browser.saveScreenshot(resolve(artifactRoot, "desktop-workspace-restore-failure.png"));
    }
    const bodyText = await $("body").getText();
    throw new Error(`the isolated workspace was not restored; visible desktop text:\n${bodyText}`, {
      cause: error,
    });
  }
});

When("I create a new desktop conversation", async () => {
  const createConversation = await $(".topbar-actions .sg-icon-button-primary");
  const previousPanel = await $(".conversation-panel");
  const previousSessionId = await previousPanel.isExisting()
    ? await previousPanel.getAttribute("data-session-id")
    : null;
  await createConversation.waitForEnabled({
    timeout: 20_000,
    timeoutMsg: "the configured local provider did not enable new conversations",
  });
  await createConversation.click();
  try {
    await browser.waitUntil(
      async () => {
        try {
          const sessionId = await $(".conversation-panel").getAttribute("data-session-id");
          const lifecycle = await $(".conversation-panel").getAttribute("data-continuity-lifecycle");
          const composerReady = await browser.execute(() => {
            const panel = document.querySelector<HTMLElement>(".conversation-panel");
            const input = panel?.querySelector<HTMLTextAreaElement>("#desktop-prompt");
            return input !== null && input !== undefined
              && !input.disabled;
          });
          const navigationSettled = await browser.execute(() =>
            document.querySelector(".conversation-loading-overlay") === null,
          );
          return sessionId !== null
            && (previousSessionId === null || sessionId !== previousSessionId)
            && lifecycle === "idle"
            && composerReady
            && navigationSettled;
        } catch {
          return false;
        }
      },
      {
        timeout: 20_000,
        timeoutMsg: "the fresh conversation composer was not enabled",
      },
    );
  } catch (error) {
    const state = await browser.execute(() => Array.from(document.querySelectorAll<HTMLElement>(".conversation-panel")).map((panel) => ({
      sessionId: panel.dataset.sessionId,
      lifecycle: panel.dataset.continuityLifecycle,
      inputDisabled: panel.querySelector<HTMLTextAreaElement>("#desktop-prompt")?.disabled,
      submitDisabled: panel.querySelector<HTMLButtonElement>(".composer-submit:not(.composer-stop)")?.disabled,
      text: panel.textContent?.slice(-400),
    })));
    throw new Error(`the fresh conversation composer was not enabled: ${JSON.stringify(state)}`, { cause: error });
  }
  await browser.waitUntil(
    async () => {
      try {
        return await browser.execute(() =>
          document.querySelector<HTMLElement>(".conversation-panel")
            ?.querySelector<HTMLTextAreaElement>("#desktop-prompt")?.value === "",
        );
      } catch {
        return false;
      }
    },
    {
      timeout: 20_000,
      timeoutMsg: "the fresh conversation composer did not settle",
    },
  );
});

Then("the conversation timeline and composer are usable", async () => {
  await $(".conversation-panel").waitForDisplayed({ timeout: 20_000 });
  await $(".timeline").waitForDisplayed();
  await $(".composer").waitForDisplayed();
  await waitForComposerEnabled();

  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  assert.ok(artifactRoot, "desktop E2E artifact directory is configured");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-workbench-smoke.png"));
});

Then("the timeline content and composer are horizontally aligned", async () => {
  const geometry = await browser.execute(() => {
    const timeline = document.querySelector<HTMLElement>(".timeline");
    const composer = document.querySelector<HTMLElement>(".composer");
    if (timeline === null || composer === null) {
      throw new Error("conversation geometry targets are unavailable");
    }
    const timelineRect = timeline.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    const timelineStyle = window.getComputedStyle(timeline);
    return {
      timelineContentLeft: timelineRect.left + Number.parseFloat(timelineStyle.paddingLeft),
      timelineContentRight: timelineRect.right - Number.parseFloat(timelineStyle.paddingRight),
      composerLeft: composerRect.left,
      composerRight: composerRect.right,
    };
  });

  assert.ok(
    Math.abs(geometry.timelineContentLeft - geometry.composerLeft) <= 1.5,
    `left edges differ: ${JSON.stringify(geometry)}`,
  );
  assert.ok(
    Math.abs(geometry.timelineContentRight - geometry.composerRight) <= 1.5,
    `right edges differ: ${JSON.stringify(geometry)}`,
  );
});

When("I start a run that requires approval", async () => {
  await browser.execute(() => {
    const scope = globalThis as typeof globalThis & { __sigilE2eErrors?: string[] };
    scope.__sigilE2eErrors = [];
    window.addEventListener("error", (event) => {
      scope.__sigilE2eErrors?.push(`${event.message}\n${event.error?.stack ?? ""}`);
    });
    window.addEventListener("unhandledrejection", (event) => {
      scope.__sigilE2eErrors?.push(String(event.reason?.stack ?? event.reason));
    });
  });
  await setComposerPrompt("请验证 Desktop 在审批等待期间仍可排队消息，并完成恢复。");
  await browser.keys("Enter");
  try {
    await $(".approval-dock").waitForDisplayed({
      timeout: 30_000,
      timeoutMsg: "the real runtime did not surface its approval request",
    });
  } catch (error) {
    const [currentUrl, title, windowHandles, pageSource] = await Promise.all([
      browser.getUrl(),
      browser.getTitle(),
      browser.getWindowHandles(),
      browser.getPageSource(),
    ]);
    const state = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      let continuity: unknown;
      let attachment: unknown;
      try {
        continuity = await invoke?.("desktop_continuity", {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
        });
        const owner = (continuity as {
          foregroundOwner?: { runId?: string; ownerRevision?: string };
        } | undefined)?.foregroundOwner;
        if (owner?.runId !== undefined && owner.ownerRevision !== undefined) {
          attachment = await invoke?.("desktop_attach_run", {
            workspaceId: panel?.dataset.workspaceId,
            input: {
              sessionId: panel?.dataset.sessionId,
              runId: owner.runId,
              ownerRevision: owner.ownerRevision,
            },
          });
        }
      } catch (diagnosticError) {
        attachment = { error: String(diagnosticError) };
      }
      return {
        body: document.body.innerText,
        html: document.documentElement.outerHTML.slice(0, 4_000),
        errors: (globalThis as typeof globalThis & { __sigilE2eErrors?: string[] })
          .__sigilE2eErrors,
        continuity,
        attachment,
      };
    });
    throw new Error(
      `the real runtime did not surface its approval request: ${JSON.stringify({
        currentUrl,
        title,
        windowHandles,
        pageSource: pageSource.slice(0, 4_000),
        state,
      })}`,
      { cause: error },
    );
  }
});

Then("the approval remains live without losing runtime control", async () => {
  await browser.pause(12_000);
  const approvalDock = await $(".approval-dock");
  await approvalDock.waitForDisplayed();
  const approvalText = await approvalDock.getText();
  assert.ok(
    approvalText.includes("file_write"),
    `approval did not expose its structured file effect: ${approvalText}`,
  );
  assert.ok(
    approvalText.includes("filesystem=workspace_write"),
    `approval did not expose its requested filesystem containment: ${approvalText}`,
  );
  assert.equal(
    await $$(".notification-toast").some(async (toast) =>
      (await toast.getText()).includes("无法连接实时运行控制")
      || (await toast.getText()).includes("Unable to connect to live runtime controls")),
    false,
    "live runtime control entered an error state while approval was pending",
  );
  await waitForComposerEnabled();

  const geometry = await browser.execute(() => {
    const approval = document.querySelector<HTMLElement>(".approval-dock");
    const composer = document.querySelector<HTMLElement>(".composer");
    if (approval === null || composer === null) {
      throw new Error("approval geometry targets are unavailable");
    }
    const approvalRect = approval.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    return {
      approvalLeft: approvalRect.left,
      approvalRight: approvalRect.right,
      composerLeft: composerRect.left,
      composerRight: composerRect.right,
    };
  });
  assert.ok(
    Math.abs(geometry.approvalLeft - geometry.composerLeft) <= 1.5,
    `approval and composer left edges differ: ${JSON.stringify(geometry)}`,
  );
  assert.ok(
    Math.abs(geometry.approvalRight - geometry.composerRight) <= 1.5,
    `approval and composer right edges differ: ${JSON.stringify(geometry)}`,
  );

  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  assert.ok(artifactRoot, "desktop E2E artifact directory is configured");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-approval.png"));
});

When("I press Enter with a follow-up while approval is pending", async () => {
  await setComposerPrompt(desktopProviderCanaries.queuedPrompt);
  await browser.keys("Enter");
});

Then("the follow-up is recorded in the durable queue", async () => {
  const queueCount = await $(".composer-queue-count");
  await queueCount.waitUntil(
    async () => (await queueCount.getText()) === "1",
    {
      timeout: 20_000,
      timeoutMsg: "pressing Enter did not record the active-run follow-up",
    },
  );
  await browser.waitUntil(
    async () => (await $("#desktop-prompt").getValue()) === "",
    {
      timeout: 5_000,
      timeoutMsg: `the queued follow-up remained in the composer draft: ${JSON.stringify(await browser.execute(() => ({
        value: document.querySelector<HTMLTextAreaElement>("#desktop-prompt")?.value ?? null,
        draft: window.localStorage.getItem("sigil:conversation-draft:v1:"),
        body: (document.body.innerText ?? "").slice(-2_000),
        errors: (globalThis as typeof globalThis & { __sigilE2eErrors?: string[] }).__sigilE2eErrors ?? [],
      })))}`,
    },
  );
});

When("I approve the pending command", async () => {
  const approve = await $(".approval-actions .sg-button-primary");
  await approve.waitForEnabled();
  await approve.click();
});

Then("the accepted approval stops offering actions before the next run event", async () => {
  await browser.waitUntil(
    async () => {
      const actions = await $$(".approval-actions button");
      const approval = await $(".approval-dock");
      if (await actions.length !== 0) return false;
      if (!(await approval.isExisting())) return true;
      const text = await approval.getText();
      return text.includes("正在恢复")
        || text.includes("已接受")
        || text.includes("Resuming")
        || text.includes("accepted");
    },
    {
      timeout: 5_000,
      timeoutMsg: "the accepted approval kept its decision actions after the command receipt",
    },
  );
});

Then("the approved action and queued follow-up both settle", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.queuedRun),
    {
      timeout: 30_000,
      timeoutMsg: "the durable follow-up did not dispatch after the active run",
    },
  );
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  assert.equal(
    readFileSync(resolve(runtimeRoot, "workspace", "desktop-e2e-approved.txt"), "utf8"),
    "desktop approval accepted\n",
    "the approved tool action did not settle before the queued follow-up answer",
  );
  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  assert.ok(artifactRoot, "desktop E2E artifact directory is configured");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-real-run.png"));
});

Then("terminal completion releases continuity and history controls", async () => {
  try {
    await waitForComposerEnabled(
      "terminal completion left the Desktop composer blocked by continuity recovery",
      45_000,
    );
    await browser.waitUntil(
      async () =>
        !(await $(".conversation-continuity-loading").isExisting())
        && !(await $(".session-rail .error-card").isExisting()),
      {
        timeout: 20_000,
        timeoutMsg: "terminal completion left continuity or conversation history in recovery",
      },
    );
    await $("#desktop-prompt").waitForEnabled();
  } catch {
    const state = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const tauri = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__;
      let serverContinuity: unknown;
      let serverDisplay: unknown;
      try {
        serverContinuity = await tauri?.core?.invoke?.("desktop_continuity", {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
        });
        serverDisplay = await (async () => {
          const historyInvoke = tauri?.core?.invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
          request: { limit: 50 },
        };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })();
      } catch (error) {
        serverContinuity = { error: String(error) };
      }
      return {
        continuity: document.querySelector(".conversation-continuity-loading")?.textContent ?? null,
        historyError: document.querySelector(".session-rail .error-card")?.textContent ?? null,
        composerDisabled: (document.querySelector("#desktop-prompt") as HTMLTextAreaElement | null)?.disabled ?? null,
        continuityLifecycle: panel?.dataset.continuityLifecycle ?? null,
        continuityRefreshState: panel?.dataset.continuityRefreshState ?? null,
        continuityOwnerRunId: panel?.dataset.continuityOwnerRunId ?? null,
        continuityPendingTerminalRunId: panel?.dataset.continuityPendingTerminalRunId ?? null,
        notices: [...document.querySelectorAll(".notification-toast")]
          .map((notice) => notice.textContent),
        serverContinuity,
        serverDisplay,
      };
    });
    const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
    if (artifactRoot !== undefined) {
      await browser.saveScreenshot(resolve(artifactRoot, "desktop-terminal-recovery-failure.png"));
    }
    throw new Error(`terminal completion did not settle: ${JSON.stringify(state)}`);
  }
});

Then("Core shows the successful file write with its durable approval and session seal", async () => {
  assert.equal(process.env.SIGIL_DESKTOP_E2E_PROFILE, "core");
  await browser.waitUntil(
    async () => (await $(".conversation-panel").getAttribute("data-continuity-lifecycle")) === "idle",
    { timeout: 30_000, timeoutMsg: "the Core conversation did not settle both runs" },
  );
  const successfulCommand = await $('.tool-card.tool-tone-success[aria-label^="Bash:"]');
  await successfulCommand.waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "Core did not visibly retain the successful ordinary file-write command",
  });
  const timelineText = await $(".timeline").getText();
  assert.ok(timelineText.includes(desktopProviderCanaries.initialRun));
  assert.ok(timelineText.includes(desktopProviderCanaries.queuedRun));

  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  const sessionId = await $(".conversation-panel").getAttribute("data-session-id");
  assert.ok(runtimeRoot && artifactRoot && sessionId, "the Core fixture identity is available");
  assert.equal(
    readFileSync(resolve(runtimeRoot, "workspace", "desktop-e2e-approved.txt"), "utf8"),
    "desktop approval accepted\n",
  );
  interface CoreRecord {
    session_id?: string;
    event_type?: string;
    payload?: {
      record?: string;
      run_id?: string;
      status?: string;
      final_message_id?: string;
      run_status?: string;
      session_log_entry?: {
        control?: Record<string, unknown>;
        tool_result_v3?: {
          schema_version?: number;
          call_id?: string;
          tool_name?: string;
          facts?: { status?: string; exit_code?: number };
          wire_semantics?: { outcome?: string };
        };
      };
    };
  }
  const allRecords = filesUnder(runtimeRoot)
    .filter((path) => path.endsWith(".jsonl"))
    .flatMap((path) => readFileSync(path, "utf8").split("\n")
      .filter((line) => line.trim() !== "")
      .map((line) => JSON.parse(line) as CoreRecord));
  // Desktop exposes an opaque handle, not the durable stream's session scope.
  // The fixture's unique tool call connects the visible approval to its durable stream.
  const toolRecords = allRecords.filter((record) =>
    record.payload?.session_log_entry?.tool_result_v3?.call_id
      === desktopProviderCanaries.approvalCallId);
  assert.equal(toolRecords.length, 1, "the visible fixture call has one durable result");
  const durableSessionId = toolRecords[0]?.session_id;
  assert.ok(durableSessionId, "the tool result identifies its durable session stream");
  const records = allRecords.filter((record) => record.session_id === durableSessionId);
  const controls = records.flatMap((record) => {
    const control = record.payload?.session_log_entry?.control;
    return control === undefined ? [] : [control];
  });
  const seals = controls.flatMap((control) =>
    control.session_composition_bound === undefined ? [] : [control.session_composition_bound]);
  assert.deepEqual(seals, [{ schema_version: 1, core_contract_version: 1, capabilities: [] }]);
  const approvals = controls.flatMap((control) => {
    const value = control.tool_approval;
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? [value as Record<string, unknown>]
      : [];
  }).filter((approval) => approval.call_id === desktopProviderCanaries.approvalCallId);
  assert.equal(approvals.filter((approval) => approval.action === "requested").length, 1);
  assert.equal(approvals.filter((approval) =>
    approval.action === "decision_accepted" && approval.user_decision === "approved").length, 1);
  assert.equal(approvals.filter((approval) =>
    approval.action === "resolved"
    && approval.user_decision === "approved"
    && approval.terminal_status === "approved").length, 1);
  assert.equal(new Set(approvals.map((approval) => JSON.stringify(approval.identity))).size, 1);
  assert.ok(controls.some((control) => control.conversation_input_queued !== undefined));
  const results = records.flatMap((record) => {
    const result = record.payload?.session_log_entry?.tool_result_v3;
    return result?.call_id === desktopProviderCanaries.approvalCallId ? [result] : [];
  });
  assert.equal(results.length, 1);
  assert.equal(results[0]?.schema_version, 3);
  assert.equal(results[0]?.tool_name, "bash");
  assert.equal(results[0]?.facts?.status, "ok");
  assert.equal(results[0]?.facts?.exit_code, 0);
  assert.equal(results[0]?.wire_semantics?.outcome, "success");
  const terminals = records.filter((record) => record.event_type === "run_finalized");
  const completedRuns = terminals.filter((record) =>
    record.payload?.record === "conversation_run_finalized_v1");
  const outcomes = terminals.filter((record) => record.payload?.record === undefined);
  assert.equal(completedRuns.length, 2);
  assert.equal(new Set(completedRuns.map((record) => record.payload?.run_id)).size, 2);
  assert.ok(completedRuns.every((record) => record.payload?.status === "succeeded"));
  assert.equal(outcomes.length, 2);
  assert.ok(outcomes.every((record) => record.payload?.run_status === "completed"));
  for (const terminal of completedRuns) {
    assert.ok(terminal.payload?.final_message_id);
    assert.equal(outcomes.filter((record) =>
      record.payload?.final_message_id === terminal.payload?.final_message_id).length, 1);
  }

  const expectedReadiness = [
    "ExecutionOneShot", "FileAccessInProcess", "StorageSessionLog",
    "StorageSessionLifecycle", "StorageInputHistory", "StorageSessionCatalog",
    "StorageArtifact", "StorageAdapterDurableState", "ProjectionRebuildable",
    "BorrowedNativeSave", "BorrowedConfiguration", "RecoverySurface", "BlockingGate",
  ].sort();
  const manifests = filesUnder(runtimeRoot)
    .filter((path) => path.endsWith("/.sigil-cutover-manifest.json") || path.endsWith("\\.sigil-cutover-manifest.json"))
    .map((path) => JSON.parse(readFileSync(path, "utf8")) as {
      schema_version?: number;
      selected_epoch?: string;
      composition?: unknown;
      mandatory_readiness?: Array<{ adapter: string; passed: boolean }>;
    });
  assert.ok(manifests.length > 0, "Core boot published its cutover manifest");
  for (const manifest of manifests) {
    assert.equal(manifest.schema_version, 2);
    assert.equal(manifest.selected_epoch, "NewCurrentSchema");
    assert.deepEqual(manifest.composition, { profile: "core", enhancements: [] });
    assert.deepEqual(manifest.mandatory_readiness?.map((probe) => probe.adapter).sort(), expectedReadiness);
    assert.ok(manifest.mandatory_readiness?.every((probe) => probe.passed));
  }
  writeFileSync(resolve(artifactRoot, "desktop-core-evidence.json"), `${JSON.stringify({
    profile: "core",
    sessionId,
    durableSessionId,
    seals,
    approvalPhases: approvals.map((approval) => approval.action),
    completedRuns: completedRuns.length,
    successfulTool: "bash",
    fileWritePath: "desktop-e2e-approved.txt",
    readinessKinds: expectedReadiness,
    provider: "loopback scripted fixture; no live-provider inference",
  }, null, 2)}\n`, "utf8");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-core-completed.png"));
});

Then("the generated semantic title is synchronized into the conversation page", async () => {
  const title = await $("#conversation-title");
  await title.waitUntil(
    async () => (await title.getText()) === desktopProviderCanaries.title,
    {
      timeout: 30_000,
      timeoutMsg: "the durable generated title was not projected into the active page",
    },
  );
});

When("I invoke the custom workspace skill", async () => {
  await waitForComposerEnabled();
  await setComposerPrompt("$desktop-e2e-skill inspect README");
  await browser.keys("Enter");
});

Then("the custom workspace skill executes with durable load evidence", async () => {
  const timeline = await $(".timeline");
  try {
    await timeline.waitUntil(
      async () => (await timeline.getText()).includes(desktopProviderCanaries.skillRun),
      {
        timeout: 120_000,
        timeoutMsg: "the provider did not execute with the loaded workspace skill",
      },
    );
  } catch {
    const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
    if (artifactRoot !== undefined) {
      await browser.saveScreenshot(resolve(artifactRoot, "desktop-skill-failure.png"));
    }
    throw new Error(
      `the provider completed the workspace skill, but the Desktop timeline did not project it:\n${await timeline.getText()}`,
    );
  }
  await browser.waitUntil(
    async () => durableEvents().includes("skill_loaded"),
    {
      timeout: 10_000,
      timeoutMsg: "the workspace skill run did not append durable skill_loaded evidence",
    },
  );
});

When("I invoke the custom workspace agent", async () => {
  await waitForComposerEnabled();
  await setComposerPrompt("@desktop-e2e-agent inspect README");
  await browser.keys("Enter");
});

Then("the custom workspace agent executes with its profile instructions", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.agentRun),
    {
      timeout: 30_000,
      timeoutMsg: `the custom workspace agent did not execute its profile instructions: ${JSON.stringify(await browser.execute(() => ({
        body: document.body.textContent ?? "",
        errors: (globalThis as typeof globalThis & { __sigilE2eErrors?: string[] }).__sigilE2eErrors ?? [],
      })))}`,
    },
  );
});

When("I read a large workspace artifact from Desktop", async () => {
  await setComposerPrompt(desktopProviderCanaries.artifactPrompt);
  await browser.keys("Enter");
});

Then("Desktop pages the canonical saved tool output", async () => {
  const savedOutput = await $('[aria-label="Read file saved output"]');
  try {
    await savedOutput.waitForDisplayed({
      timeout: 30_000,
      timeoutMsg: "the real runtime did not expose the large read_file artifact",
    });
  } catch (error) {
    const diagnostic = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      let display: unknown;
      let continuity: unknown;
      let runContext: unknown;
      try {
        const input = {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
        };
        continuity = await invoke?.("desktop_continuity", input);
        runContext = await invoke?.("desktop_run_context", input);
        display = await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = {
          ...input,
          request: { limit: 50 },
        };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })();
      } catch (displayError) {
        display = { error: String(displayError) };
      }
      return {
        body: (document.body.textContent ?? "").slice(-5000),
        display,
      };
    });
    throw new Error(`the real runtime did not expose the large read_file artifact: ${JSON.stringify(diagnostic)}`, {
      cause: error,
    });
  }
  const view = await savedOutput.$("button=View saved output");
  await view.waitForEnabled();
  await view.click();
  try {
    await browser.waitUntil(
      async () => await browser.execute(() => document
        .querySelector<HTMLElement>('[aria-label="read_file saved output page"]')
        ?.textContent?.includes("DESKTOP_E2E_ARTIFACT_PAGE_ONE") === true),
      { timeout: 20_000, timeoutMsg: "the first canonical artifact page was not rendered" },
    );
  } catch (error) {
    const diagnostic = await browser.execute(() => ({
      body: (document.body.textContent ?? "").slice(-5000),
      cards: Array.from(document.querySelectorAll<HTMLElement>(".tool-card")).map((card) => card.textContent?.slice(0, 500)),
    }));
    throw new Error(`the first canonical artifact page was not rendered: ${JSON.stringify(diagnostic)}`, {
      cause: error,
    });
  }
  const nextState = await browser.execute(() => {
    const page = document.querySelector<HTMLElement>('[aria-label="read_file saved output page"]');
    const section = page?.closest<HTMLElement>(".tool-artifact-retrieval")
      ?? document.querySelector<HTMLElement>(".tool-artifact-retrieval");
    const buttons = Array.from(section?.querySelectorAll<HTMLButtonElement>("button") ?? [])
      .map((button) => ({
        text: button.textContent,
        ariaLabel: button.getAttribute("aria-label"),
        disabled: button.disabled,
      }));
    const next = Array.from(section?.querySelectorAll<HTMLButtonElement>("button") ?? [])
      .find((button) => button.textContent?.includes("Next page") || button.getAttribute("aria-label")?.includes("Next"));
    if (next === undefined || next.disabled) return { clicked: false, buttons };
    next.click();
    return { clicked: true, buttons };
  });
  assert.equal(
    nextState.clicked,
    true,
    `the first canonical artifact page did not expose an enabled next action: ${JSON.stringify(nextState.buttons)}`,
  );
  await browser.waitUntil(
    async () => await browser.execute(() => document
      .querySelector<HTMLElement>('[aria-label="read_file saved output page"]')
      ?.textContent?.includes("DESKTOP_E2E_ARTIFACT_PAGE_TWO") === true),
    { timeout: 20_000, timeoutMsg: "the second canonical artifact page was not rendered" },
  );
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.artifactFinal),
    { timeout: 30_000, timeoutMsg: "the artifact-backed run did not reach terminal output" },
  );
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const durableSource = readFileSync(
    resolve(runtimeRoot, "workspace", "desktop-e2e-large-output.txt"),
    "utf8",
  );
  assert.ok(durableSource.includes("DESKTOP_E2E_ARTIFACT_PAGE_ONE"));
  assert.ok(durableSource.includes("DESKTOP_E2E_ARTIFACT_PAGE_TWO"));
});

When("I reload Desktop after the saved output settles", async () => {
  await browser.refresh();
  await $(".app-shell").waitForDisplayed({ timeout: 20_000 });
});

Then("Desktop restores the artifact-backed tool card", async () => {
  const workspace = await $(".workspace-switcher");
  await workspace.waitUntil(
    async () => (await workspace.getText()).includes("desktop-e2e-workspace"),
    { timeout: 20_000, timeoutMsg: "Desktop did not restore the artifact workspace" },
  );
  const savedOutput = await $('[aria-label="Read file saved output"]');
  await savedOutput.waitForDisplayed({
    timeout: 30_000,
    timeoutMsg: "Desktop did not restore the artifact-backed tool card",
  });
  const view = await savedOutput.$("button=View saved output");
  await view.waitForEnabled();
  await view.click();
  const savedPage = await savedOutput.$('[aria-label="read_file saved output page"]');
  await savedPage.waitUntil(
    async () => {
      return (await savedPage.getText()).includes("DESKTOP_E2E_ARTIFACT_PAGE_ONE");
    },
    { timeout: 20_000, timeoutMsg: "the restored card could not reread its durable artifact" },
  );
});

When("the saved artifact body becomes unavailable outside Desktop", async () => {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const artifactBlob = filesUnder(resolve(runtimeRoot, "state"))
    .filter((path) => path.endsWith(".blob"))
    .find((path) => readFileSync(path, "utf8").includes("DESKTOP_E2E_ARTIFACT_PAGE_ONE"));
  assert.ok(artifactBlob, "the canonical large-output artifact blob was not found");
  unlinkSync(artifactBlob);
  assert.equal(existsSync(artifactBlob), false, "the external artifact removal did not settle");
  await browser.refresh();
  await $(".app-shell").waitForDisplayed({ timeout: 20_000 });
});

Then("Desktop disables artifact retrieval while retaining the auditable card", async () => {
  const savedOutput = await $('[aria-label="Read file saved output"]');
  assert.equal(await savedOutput.isExisting(), false, "a missing artifact still exposed retrieval controls");
  const card = await $(".tool-card*=DESKTOP_E2E_ARTIFACT_PAGE_ONE");
  await card.waitForDisplayed({
    timeout: 30_000,
    timeoutMsg: "Desktop discarded the bounded tool summary after the artifact became unavailable",
  });
  const cardText = await card.getText();
  assert.ok(
    cardText.includes("Saved output is not currently available.")
      || cardText.includes("已保存输出当前不可用。"),
    "Desktop did not explain that the saved output is unavailable",
  );
});

When("I run a failing artifact-backed shell command", async () => {
  await browser.pause(1_000);
  await browser.execute((value) => {
    const input = document.querySelector<HTMLTextAreaElement>("#desktop-prompt");
    if (input === null) throw new Error("the desktop composer disappeared before input");
    input.focus({ preventScroll: true });
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    if (setter === undefined) throw new Error("the desktop composer value setter is unavailable");
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }, desktopProviderCanaries.shellErrorPrompt);
  await browser.waitUntil(
    async () => await browser.execute(
      (value) => document.querySelector<HTMLTextAreaElement>("#desktop-prompt")?.value === value,
      desktopProviderCanaries.shellErrorPrompt,
    ),
    { timeout: 5_000, timeoutMsg: "the failing shell prompt was not retained by the composer" },
  );
  await browser.waitUntil(
    async () => await $(".composer-submit:not(.composer-stop)").isEnabled(),
    { timeout: 5_000, timeoutMsg: "the failing shell prompt did not enable the send action" },
  );
  await browser.keys("Enter");
  try {
    await $(".approval-dock").waitForDisplayed({
      timeout: 30_000,
      timeoutMsg: "the failing shell command did not reach the real approval boundary",
    });
  } catch (error) {
    const diagnostic = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const prompt = document.querySelector<HTMLTextAreaElement>("#desktop-prompt");
      const send = document.querySelector<HTMLButtonElement>(".composer-submit:not(.composer-stop)");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input?: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      let continuity: unknown;
      try {
        continuity = await invoke?.("desktop_continuity", {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
        });
      } catch (continuityError) {
        continuity = { error: String(continuityError) };
      }
      return {
        panel: panel === null ? undefined : {
          sessionId: panel.dataset.sessionId,
          lifecycle: panel.dataset.continuityLifecycle,
          refreshState: panel.dataset.continuityRefreshState,
          ownerRunId: panel.dataset.continuityOwnerRunId,
        },
        prompt: prompt === null ? undefined : {
          value: prompt.value,
          disabled: prompt.disabled,
        },
        send: send === null ? undefined : {
          disabled: send.disabled,
          ariaBusy: send.getAttribute("aria-busy"),
        },
        continuity,
        body: document.body.innerText.slice(-4_000),
      };
    });
    throw new Error(`the failing shell command did not reach the real approval boundary: ${JSON.stringify(diagnostic)}`, {
      cause: error,
    });
  }
});

Then("Desktop preserves the failing shell result and pages its stderr artifact", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.shellErrorFinal),
    { timeout: 30_000, timeoutMsg: "the agent loop did not remain usable after the shell failure" },
  );
  let artifactCardEvidence: {
    card: { clicked: boolean; text: string } | null;
    cardsSummary: Array<{ ariaLabel: string | null; hasArtifact: boolean; text: string }>;
  } | undefined;
  await browser.waitUntil(async () => {
    artifactCardEvidence = await browser.execute((canary) => {
      const cards = Array.from(document.querySelectorAll<HTMLElement>(".tool-card"));
      const card = cards.find((candidate) =>
        candidate.textContent?.includes(canary)
        && candidate.querySelector(".tool-artifact-retrieval") !== null
      );
      const cardsSummary = cards.map((candidate) => ({
        ariaLabel: candidate.getAttribute("aria-label"),
        hasArtifact: candidate.querySelector(".tool-artifact-retrieval") !== null,
        text: (candidate.textContent ?? "").slice(0, 300),
      }));
      if (card === undefined) return { card: null, cardsSummary };
      const view = Array.from(card.querySelectorAll<HTMLButtonElement>("button"))
        .find((button) => button.textContent?.includes("View saved output"));
      view?.click();
      return {
        card: {
          clicked: view !== undefined,
          text: card.textContent ?? "",
        },
        cardsSummary,
      };
    }, desktopProviderCanaries.shellErrorOutput);
    return artifactCardEvidence.card !== null;
  }, {
    timeout: 30_000,
    timeoutMsg: "the failed shell card never received its durable artifact projection",
  });
  assert.ok(artifactCardEvidence);
  const artifactCard = artifactCardEvidence.card;
  assert.ok(
    artifactCard,
    `the failed real shell command did not expose its saved stderr artifact: ${JSON.stringify(artifactCardEvidence.cardsSummary)}`,
  );
  assert.equal(artifactCard.clicked, true, "the saved stderr retrieval action was unavailable");
  await browser.waitUntil(
    async () => await browser.execute((canary) => document
      .querySelector<HTMLElement>('[aria-label="bash saved output page"]')
      ?.textContent?.includes(canary) === true, desktopProviderCanaries.shellErrorOutput),
    { timeout: 20_000, timeoutMsg: "the canonical stderr artifact omitted its failure canary" },
  );
  assert.match(artifactCard.text, /failed|error|exit code 7/i);
});

When("I start a persistent terminal task from Desktop", async () => {
  await waitForComposerEnabled();
  await setComposerPrompt(desktopProviderCanaries.terminalLifecyclePrompt);
  await submitComposerPrompt();

  await browser.waitUntil(
    async () => await browser.execute(() => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      return panel?.querySelector(".approval-dock, .terminal-task-card") !== null;
    }),
    {
      timeout: 30_000,
      timeoutMsg: "the persistent terminal request produced neither approval nor a task card",
    },
  );
  const approval = await $(".conversation-panel .approval-dock");
  if (await approval.isExisting()) {
    await approval.$(".approval-actions .sg-button-primary").click();
  }
});

Then("the terminal task becomes ready before the foreground answer completes", async () => {
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  await card.waitForDisplayed({
    timeout: 30_000,
    timeoutMsg: "Desktop did not render the persistent terminal task card",
  });
  await browser.waitUntil(
    async () => await browser.execute(() => {
      const current = document.querySelector<HTMLElement>(
        ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
      );
      return current?.dataset.terminalTaskReadiness === "ready"
        && current.dataset.terminalTaskStatus === "running";
    }),
    {
      timeout: 20_000,
      timeoutMsg: "the terminal task did not reach ready/running",
    },
  );
  const cardText = await browser.execute(() => document.querySelector<HTMLElement>(
    ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
  )?.textContent ?? "");
  assert.ok(
    cardText.includes(desktopProviderCanaries.terminalLifecycleReady) === false,
    "the bounded card leaked raw terminal output instead of lifecycle facts",
  );
});

Then("the foreground answer completes while the terminal task remains running", async () => {
  await browser.waitUntil(
    async () => await browser.execute((canary) => document
      .querySelector<HTMLElement>(".timeline")
      ?.textContent?.includes(canary) === true, desktopProviderCanaries.terminalLifecycleFinal),
    {
      timeout: 30_000,
      timeoutMsg: "the foreground answer did not complete after terminal readiness",
    },
  );
  const status = await browser.execute(() => document.querySelector<HTMLElement>(
    ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
  )?.dataset.terminalTaskStatus);
  assert.equal(status, "running");
});

When("I start a successor run while the older terminal task remains running", async () => {
  await waitForComposerEnabled("the composer did not recover after foreground completion");
  await setComposerPrompt(desktopProviderCanaries.terminalSuccessorPrompt);
  await submitComposerPrompt();
});

Then("the successor completes while the older terminal task is still tracked", async () => {
  await browser.waitUntil(
    async () => await browser.execute((canary) => document
      .querySelector<HTMLElement>(".timeline")
      ?.textContent?.includes(canary) === true, desktopProviderCanaries.terminalSuccessorFinal),
    {
      timeout: 20_000,
      timeoutMsg: "the successor run did not complete while the older terminal owner was retained",
    },
  );
  const status = await browser.execute(() => document.querySelector<HTMLElement>(
    ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
  )?.dataset.terminalTaskStatus);
  assert.equal(
    status,
    "running",
    "the successor run displaced the older terminal owner before it settled",
  );
});

When("I stop the retained terminal task", async () => {
  const stop = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task'] button");
  await stop.waitForEnabled({
    timeout: 10_000,
    timeoutMsg: "the retained terminal task did not expose its stop control",
  });
  await stop.click();
});

Then("the retained terminal task becomes cancelled", async () => {
  await browser.waitUntil(
    async () => await browser.execute(() => document.querySelector<HTMLElement>(
      ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
    )?.dataset.terminalTaskStatus === "cancelled"),
    {
      timeout: 20_000,
      timeoutMsg: "the retained terminal task did not reach cancelled",
    },
  );
  await browser.refresh();
  await $(".app-shell").waitForDisplayed({ timeout: 20_000 });
  await waitForComposerEnabled("the composer did not recover after cancelling the retained terminal task");
  await browser.waitUntil(
    async () => await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      if (invoke === undefined || panel?.dataset.workspaceId === undefined || panel.dataset.sessionId === undefined) {
        return false;
      }
      const continuity = await invoke("desktop_continuity", {
        workspaceId: panel.dataset.workspaceId,
        sessionId: panel.dataset.sessionId,
      }) as {
        retainedTerminalRuns?: Array<{
          terminalTasks?: Array<{ status?: unknown }>;
        }>;
      };
      const hasActiveRetainedTask = continuity.retainedTerminalRuns?.some((run) => (
        run.terminalTasks?.some((task) => task.status === "starting" || task.status === "running")
      )) ?? false;
      return (panel.dataset.continuityLifecycle === "idle" || panel.dataset.continuityLifecycle === "live")
        && Array.isArray(continuity.retainedTerminalRuns)
        && !hasActiveRetainedTask;
    }),
    {
      timeout: 30_000,
      timeoutMsg: "the cancelled terminal task still had an active retained backend task after refresh",
    },
  );
});

When("I reload Desktop while the retained terminal task is owned by the session", async () => {
  await browser.refresh();
});

Then("Desktop restores the retained terminal task from continuity", async () => {
  await $(".app-shell").waitForDisplayed({ timeout: 20_000 });
  const workspace = await $(".workspace-switcher");
  await workspace.waitUntil(
    async () => (await workspace.getText()).includes("desktop-e2e-workspace"),
    {
      timeout: 20_000,
      timeoutMsg: "Desktop did not restore the isolated workspace after renderer reload",
    },
  );
  await browser.waitUntil(
    async () => await browser.execute(() => document.querySelector<HTMLElement>(
      ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
    ) !== null),
    {
      timeout: 20_000,
      timeoutMsg: "continuity did not restore the retained terminal task card",
    },
  );
  const restored = await browser.execute(() => {
    const card = document.querySelector<HTMLElement>(
      ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
    );
    return {
      status: card?.dataset.terminalTaskStatus,
      generation: card?.dataset.terminalTaskGeneration,
    };
  });
  const status = restored.status;
  assert.ok(
    status === "running" || status === "exited",
    `continuity restored an invalid terminal status: ${status}`,
  );
  const generation = Number.parseInt(restored.generation ?? "0", 10);
  assert.ok(generation > 0, "continuity did not restore a generation-bound terminal owner");
  assert.equal(
    await $(".conversation-continuity-loading").isExisting(),
    false,
    "continuity hydration left the conversation in recovery",
  );
});

When("I start a provider stream with one hundred thousand short deltas", async () => {
  await resetProviderEvidence();
  await waitForComposerEnabled();
  await setComposerPrompt(desktopProviderCanaries.highDeltaPrompt);
  await submitComposerPrompt();
});

Then("Desktop displays one bounded truncated preview while the provider is paused", async () => {
  await browser.waitUntil(
    async () => (await providerEvidence()).requestCounts.high_delta_frames === 100_000,
    { timeout: 45_000, timeoutMsg: "provider did not send all 100000 short deltas" },
  );
  await browser.waitUntil(
    async () => await browser.execute(() => {
      const previews = [...document.querySelectorAll<HTMLElement>(
        ".timeline .message-progress[open] .message-content",
      )];
      const text = previews[0]?.textContent ?? "";
      return previews.length === 1 && text.includes("x".repeat(65_536))
        && text.includes("…") && !text.includes("x".repeat(65_537));
    }),
    { timeout: 30_000, timeoutMsg: "Desktop did not keep one truncated 64 KiB replacement preview" },
  );
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.high_delta_stream, 1);
});

When("I release the paused provider final answer", async () => {
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const response = await fetch(`${fixtureBaseUrl}/__release-high-delta`, { method: "POST" });
  assert.equal(response.ok, true, "the paused provider final answer was not released");
});

Then("Desktop displays exactly one complete high-delta final answer", async () => {
  await assertCompleteHighDeltaAnswer();
});

When("I reload Desktop after the high-delta answer settles", async () => {
  await browser.refresh();
});

Then("Desktop restores exactly one complete high-delta final answer", async () => {
  await assertCompleteHighDeltaAnswer();
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.high_delta_stream, 1, "reload started another provider stream");
});

async function assertCompleteHighDeltaAnswer(): Promise<void> {
  await browser.waitUntil(
    async () => await browser.execute(() => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const rows = [...document.querySelectorAll<HTMLElement>(".timeline .message-assistant")];
      const finals = rows.filter((row) => row.textContent?.includes("x".repeat(64)));
      return panel?.dataset.continuityLifecycle === "idle" && finals.length === 1
        && finals[0]?.querySelector("[data-message-content-toggle]") !== null
        && document.querySelector(".timeline .message-status-streaming") === null;
    }),
    { timeout: 45_000, timeoutMsg: "Desktop did not replace the live preview with one saved answer" },
  );
  const toggle = await $(".timeline .message-assistant [data-message-content-toggle]");
  await toggle.click();
  let offset = 0;
  let complete = "";
  for (let page = 0; page < 4; page += 1) {
    await browser.waitUntil(async () => await browser.execute((expected) => {
      const content = document.querySelector<HTMLElement>("[data-message-content-page]");
      return content?.dataset.contentOffset === String(expected);
    }, offset), { timeout: 20_000, timeoutMsg: "Desktop did not load the next saved answer page" });
    const current = await browser.execute(() => {
      const pages = document.querySelectorAll<HTMLElement>("[data-message-content-page]");
      const content = pages[0];
      return { count: pages.length, text: content?.textContent ?? "",
        next: content?.dataset.contentNextOffset ?? "" };
    });
    assert.equal(current.count, 1, "Desktop accumulated message pages in the DOM");
    assert.ok(Buffer.byteLength(current.text, "utf8") <= 65_536);
    complete += current.text;
    if (current.next === "") break;
    offset = Number(current.next);
    await $("[data-message-content-next]").click();
  }
  assert.equal(complete, `${"x".repeat(100_000)}\n\n${desktopProviderCanaries.highDeltaFinal}`);
  assert.equal(await $$(".timeline .message-assistant").length, 1);
  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  if (artifactRoot !== undefined) await browser.saveScreenshot(resolve(artifactRoot, "high-delta-complete-message.png"));
  await toggle.click();
}

async function resetProviderEvidence(): Promise<void> {
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const reset = await fetch(`${fixtureBaseUrl}/__reset-evidence`, { method: "POST" });
  assert.equal(reset.ok, true, "desktop E2E provider evidence could not be reset");
}

When("an Agent asks a durable question from Desktop", async () => {
  durableUserInputIdentity = undefined;
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const reset = await fetch(`${fixtureBaseUrl}/__reset-evidence`, { method: "POST" });
  assert.equal(reset.ok, true, "desktop E2E provider evidence could not be reset");

  await waitForComposerEnabled();
  await setComposerPrompt(desktopProviderCanaries.durableUserInputPrompt);
  await submitComposerPrompt();
  try {
    await browser.waitUntil(
      async () => await browser.execute(async () => {
        const panel = document.querySelector<HTMLElement>(".conversation-panel");
        const workspaceId = panel?.dataset.workspaceId;
        const sessionId = panel?.dataset.sessionId;
        const invoke = (
          globalThis as typeof globalThis & {
            __TAURI__?: {
              core?: {
                invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
              };
            };
          }
        ).__TAURI__?.core?.invoke;
        if (invoke === undefined || workspaceId === undefined || sessionId === undefined) {
          return false;
        }
        const display = await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = {
          workspaceId,
          sessionId,
          request: { limit: 50 },
        };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })() as { userInputs?: unknown[] };
        return Array.isArray(display.userInputs) && display.userInputs.length > 0;
      }),
      {
        timeout: 30_000,
        timeoutMsg: "Desktop did not durably project the question before reload",
      },
    );
  } catch (error) {
    const diagnostic = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      const input = {
        workspaceId: panel?.dataset.workspaceId,
        sessionId: panel?.dataset.sessionId,
      };
      return {
        input,
        display: invoke === undefined
          ? "bridge unavailable"
          : await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = { ...input, request: { limit: 50 } };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })(),
        continuity: invoke === undefined ? undefined : await invoke("desktop_continuity", input),
        body: document.body.textContent,
      };
    });
    const evidence = await providerEvidence();
    throw new Error(`Desktop did not durably project the question before reload: ${JSON.stringify({ diagnostic, evidence })}`, {
      cause: error,
    });
  }
  await browser.refresh();
});

Then("Desktop presents the exact unresolved question", async () => {
  const card = await $(".user-input-card");
  try {
    await card.waitForDisplayed({
      timeout: 30_000,
      timeoutMsg: "Desktop did not render the durable Agent question",
    });
  } catch (error) {
    const diagnostic = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      let continuity: unknown;
      let runContext: unknown;
      let display: unknown;
      try {
        const input = {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
        };
        continuity = await invoke?.("desktop_continuity", input);
        runContext = await invoke?.("desktop_run_context", input);
        display = await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = {
          ...input,
          request: { limit: 50 },
        };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })();
      } catch (displayError) {
        display = { error: String(displayError) };
      }
      return {
        body: document.body.textContent,
        continuityLifecycle: panel?.dataset.continuityLifecycle,
        continuityRefreshState: panel?.dataset.continuityRefreshState,
        continuity,
        runContext,
        display,
      };
    });
    throw new Error(`Desktop did not render the durable Agent question: ${JSON.stringify(diagnostic)}`, {
      cause: error,
    });
  }
  const cardText = await card.getText();
  assert.ok(cardText.includes(desktopProviderCanaries.durableUserInputCardPrompt));
  assert.ok(cardText.includes(desktopProviderCanaries.durableUserInputQuestion));

  const requested = matchingUserInputControls("user_input_requested");
  assert.equal(requested.length, 1, "the Agent question was not persisted exactly once");
  durableUserInputIdentity = requestedIdentity(requested[0]);

  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.durable_user_input_request, 1);
  assert.equal(evidence.requestCounts.durable_user_input_continuation ?? 0, 0);
});

When("I reload Desktop while the Agent question is unresolved", async () => {
  assert.ok(durableUserInputIdentity, "the unresolved Agent question identity was not captured");
  await browser.refresh();
});

Then("Desktop restores the same question without starting a continuation", async () => {
  await $(".app-shell").waitForDisplayed({ timeout: 20_000 });
  const workspace = await $(".workspace-switcher");
  await workspace.waitUntil(
    async () => (await workspace.getText()).includes("desktop-e2e-workspace"),
    {
      timeout: 20_000,
      timeoutMsg: "Desktop did not restore the workspace after the question reload",
    },
  );
  const card = await $(".user-input-card");
  await card.waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "Desktop did not restore the unresolved Agent question",
  });
  assert.ok((await card.getText()).includes(desktopProviderCanaries.durableUserInputQuestion));

  const requested = matchingUserInputControls("user_input_requested");
  assert.equal(requested.length, 1, "renderer reload duplicated the durable question");
  assert.deepEqual(requestedIdentity(requested[0]), durableUserInputIdentity);
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.durable_user_input_request, 1);
  assert.equal(evidence.requestCounts.durable_user_input_continuation ?? 0, 0);
});

When("I answer the restored Agent question", async () => {
  const card = await $(".user-input-card");
  const select = await card.$("select");
  await browser.execute((element, label) => {
    const control = element as HTMLSelectElement;
    const option = [...control.options].find((candidate) => candidate.text === label);
    if (option === undefined) throw new Error(`missing durable user-input option: ${label}`);
    control.value = option.value;
    control.dispatchEvent(new Event("change", { bubbles: true }));
  }, select, desktopProviderCanaries.durableUserInputOption);
  await select.waitUntil(
    async () => (await select.getValue()) !== "",
    {
      timeout: 5_000,
      timeoutMsg: "the restored Agent question did not retain the selected answer",
    },
  );
  const submit = await card.$(".plan-card-actions .sg-button-primary");
  await submit.waitForClickable({
    timeout: 10_000,
    timeoutMsg: "the restored Agent question did not accept an answer",
  });
  await submit.click();
  await browser.waitUntil(
    async () => matchingUserInputControls("user_input_decision_accepted").length === 1,
    {
      timeout: 10_000,
      timeoutMsg: `Desktop did not durably accept the restored answer: ${await card.getText()}`,
    },
  );
});

Then("Desktop resumes exactly one continuation and completes the answer", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.durableUserInputFinal),
    {
      timeout: 30_000,
      timeoutMsg: "Desktop did not complete the durable user-input continuation",
    },
  );
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.durable_user_input_request, 1);
  assert.equal(evidence.requestCounts.durable_user_input_continuation, 1);
});

Then("the durable user-input lifecycle is fully settled", async () => {
  assert.ok(durableUserInputIdentity, "the durable Agent question identity is unavailable");
  await browser.waitUntil(
    async () => matchingUserInputControls("user_input_resolved").length === 1,
    {
      timeout: 10_000,
      timeoutMsg: "Desktop rendered the continuation answer before its durable input resolution settled",
    },
  );
  assert.equal(matchingUserInputControls("user_input_requested").length, 1);
  assert.equal(matchingUserInputControls("user_input_decision_accepted").length, 1);
  assert.equal(matchingUserInputControls("user_input_continuation_claimed").length, 1);
  assert.equal(matchingUserInputControls("user_input_continuation_started").length, 1);
  const resolved = matchingUserInputControls("user_input_resolved");
  assert.equal(resolved.length, 1);
  assert.deepEqual(resolved[0]?.resolution, { kind: "consumed" });
  await $(".user-input-card").waitForDisplayed({
    timeout: 10_000,
    reverse: true,
    timeoutMsg: "Desktop retained a resolved Agent question",
  });
});

Then("the later terminal exit settles the Desktop task card", async () => {
  await browser.waitUntil(
    async () => await browser.execute(() => document.querySelector<HTMLElement>(
      ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
    )?.dataset.terminalTaskStatus === "exited"),
    {
      timeout: 105_000,
      timeoutMsg: "Desktop stopped following terminal lifecycle after foreground completion",
    },
  );
  const restored = await browser.execute(() => {
    const card = document.querySelector<HTMLElement>(
      ".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']",
    );
    return {
      generation: card?.dataset.terminalTaskGeneration,
      text: card?.textContent ?? "",
    };
  });
  const generation = Number.parseInt(restored.generation ?? "0", 10);
  assert.ok(generation > 1, `the terminal lifecycle generation did not advance: ${generation}`);
  assert.ok(restored.text.includes("0"), "the exited task card did not show its exit code");
  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  assert.ok(artifactRoot, "desktop E2E artifact directory is configured");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-terminal-lifecycle.png"));
});

When("I invoke Desktop plan mode", async () => {
  await waitForComposerEnabled();
  await setComposerPrompt("/plan inspect the runtime architecture");
  await submitComposerPrompt();
});

Then("the supervised plan agent drafts a durable plan", async () => {
  await browser.waitUntil(
    async () => durableEvidenceContains('"source":"explicit_plan_command"'),
    {
      timeout: 30_000,
      timeoutMsg: "Desktop plan mode did not persist an explicit plan review attempt",
    },
  );
});

Then("the automatic plan review drafts a durable plan", async () => {
  await browser.waitUntil(
    async () => await browser.execute((summary) => document
      .querySelector<HTMLElement>(".timeline")
      ?.textContent?.includes(summary) === true, desktopProviderCanaries.planDraftSummary),
    {
      timeout: 45_000,
      timeoutMsg: "Desktop automatic plan review did not commit a draft plan",
    },
  );
});

Then("the draft plan becomes ready on the Desktop plan card", async () => {
  const card = await $(".plan-card");
  await card.waitForDisplayed({ timeout: 15_000 });
  await browser.waitUntil(
    async () => await browser.execute((summary) => document
      .querySelector<HTMLElement>(".plan-card")
      ?.textContent?.includes(summary) === true, desktopProviderCanaries.planDraftSummary),
    {
      timeout: 10_000,
      timeoutMsg: "Desktop did not render the ready draft plan card",
    },
  );
});

When("I revise the draft plan with clarification guidance", async () => {
  planClarificationIdentity = undefined;
  await resetProviderEvidence();
  // Resolve the current button and dispatch in the same renderer turn: the ready
  // card can be replaced between WebDriver's clickable check and pointer command.
  await browser.waitUntil(async () => await browser.execute(() => {
    const revise = document.querySelector<HTMLButtonElement>(".plan-card [data-plan-action='revise']");
    if (revise === null || !revise.isConnected || revise.disabled || revise.getClientRects().length === 0) {
      return false;
    }
    revise.click();
    return true;
  }), { timeout: 20_000, timeoutMsg: "Desktop did not expose an enabled Revise action" });
  const card = await $(".user-input-card");
  await card.waitForDisplayed({ timeout: 20_000 });
  assert.ok((await card.getText()).includes("What should change in this plan?"));
  const guidance = await card.$("textarea");
  await browser.execute((element, value) => {
    const control = element as HTMLTextAreaElement;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    if (setter === undefined) throw new Error("revision guidance input setter is unavailable");
    setter.call(control, value);
    control.dispatchEvent(new Event("input", { bubbles: true }));
  }, guidance, desktopProviderCanaries.planRevisionGuidance);
  const submit = await card.$(".plan-card-actions .sg-button-primary");
  await submit.waitForClickable({ timeout: 10_000 });
  await submit.click();
});

Then("the Plan child asks its durable verification question", async () => {
  await waitForPlanClarification();
  const requested = durableControls("user_input_requested").filter((control) => {
    const request = control.request as Record<string, unknown> | undefined;
    return request?.prompt === desktopProviderCanaries.planRevisionQuestionPrompt;
  });
  assert.equal(requested.length, 1, "Plan clarification must have one durable identity");
  planClarificationIdentity = requestedIdentity(requested[0]);
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.plan_revision_question, 1);
  assert.equal(evidence.requestCounts.plan_revision_completed ?? 0, 0);
});

When("I reload Desktop during the Plan clarification", async () => {
  assert.ok(planClarificationIdentity, "Plan clarification identity was not captured");
  await browser.refresh();
});

Then("Desktop restores the Plan clarification without resuming it", async () => {
  await waitForPlanClarification();
  const requested = durableControls("user_input_requested").filter((control) => {
    const request = control.request as Record<string, unknown> | undefined;
    return request?.prompt === desktopProviderCanaries.planRevisionQuestionPrompt;
  });
  assert.equal(requested.length, 1);
  assert.deepEqual(requestedIdentity(requested[0]), planClarificationIdentity);
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.plan_revision_question, 1);
  assert.equal(evidence.requestCounts.plan_revision_completed ?? 0, 0);
});

When("I answer the Plan verification question", async () => {
  const card = await $(".user-input-card");
  const select = await card.$("select");
  await browser.execute((element, label) => {
    const control = element as HTMLSelectElement;
    const option = [...control.options].find((candidate) => candidate.text === label);
    if (option === undefined) throw new Error(`missing Plan verification option: ${label}`);
    control.value = option.value;
    control.dispatchEvent(new Event("change", { bubbles: true }));
  }, select, desktopProviderCanaries.durableUserInputOption);
  const submit = await card.$(".plan-card-actions .sg-button-primary");
  await submit.waitForClickable({ timeout: 10_000 });
  await submit.click();
});

Then("the Plan child resumes once and replaces the draft with the revised plan", async () => {
  await browser.waitUntil(
    async () => await browser.execute((summary) => {
      const card = document.querySelector<HTMLElement>(".plan-card");
      return card?.textContent?.includes(summary) === true
        && card.querySelector(".plan-status-draft-ready") !== null
        && document.querySelector(".user-input-card") === null;
    }, desktopProviderCanaries.planRevisionSummary),
    { timeout: 45_000, timeoutMsg: "Plan clarification did not resume into the revised draft" },
  );
  const evidence = await providerEvidence();
  assert.equal(evidence.requestCounts.plan_revision_question, 1);
  assert.equal(evidence.requestCounts.plan_revision_completed, 1);
  assert.ok(planClarificationIdentity, "Plan clarification identity is unavailable");
  const resolved = durableControls("user_input_resolved").filter((control) => {
    const identity = control.identity as Record<string, unknown> | undefined;
    return identity?.request_id === planClarificationIdentity?.requestId
      && identity?.generation === planClarificationIdentity?.generation
      && control.request_hash === planClarificationIdentity?.requestHash;
  });
  assert.equal(resolved.length, 1, "Plan clarification must settle exactly once");
  assert.deepEqual(resolved[0]?.resolution, { kind: "consumed" });
});

async function waitForPlanClarification(): Promise<void> {
  await browser.waitUntil(
    async () => await browser.execute((prompt) => document
      .querySelector<HTMLElement>(".user-input-card")?.textContent?.includes(prompt) === true,
    desktopProviderCanaries.planRevisionQuestionPrompt),
    { timeout: 45_000, timeoutMsg: "Desktop did not present the durable Plan child question" },
  );
}

When("I save the reviewed plan from Desktop", async () => {
  const decision = await invokeDesktopPlanDecision("save");
  await browser.waitUntil(
    async () => await browser.execute(async ({ workspaceId, sessionId }) => {
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      if (invoke === undefined) return false;
      const display = await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = {
        workspaceId,
        sessionId,
        request: { limit: 50 },
      };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })() as {
        planReview?: { allowedActions?: string[] };
      };
      const actions = display.planReview?.allowedActions ?? [];
      return display.planReview !== undefined
        && actions.includes("run")
        && actions.includes("revise")
        && actions.includes("reject")
        && !actions.includes("save");
    }, decision),
    {
      timeout: 15_000,
      timeoutMsg: "the saved plan decision did not reach the canonical Desktop display",
    },
  );
  await browser.refresh();
});

Then("the saved plan remains available without creating a Task", async () => {
  await $(".plan-card").waitForDisplayed({ timeout: 10_000 });
  assert.equal(await $("[data-plan-action='save']").isExisting(), false);
  assert.equal(await $("[data-plan-action='run']").isExisting(), true);
  assert.equal(await $("[data-plan-action='revise']").isExisting(), true);
  assert.equal(await $("[data-plan-action='reject']").isExisting(), true);
  await browser.waitUntil(
    async () => !durableControls("task_step").some((control) => control.status === "started"),
    {
      timeout: 10_000,
      timeoutMsg: "Saving the reviewed plan unexpectedly created a Task",
    },
  );
});

When("I run the reviewed plan from Desktop", async () => {
  const decision = await invokeDesktopPlanDecision("run");
  assert.equal(typeof decision.taskId, "string", "Desktop plan Run did not return a Task identity");
  directLifecycleBinding = { workspaceId: decision.workspaceId, sessionId: decision.sessionId, taskId: String(decision.taskId), admissionId: "" };
  await browser.execute(async ({ workspaceId, sessionId, taskId }) => {
    const invoke = (
      globalThis as typeof globalThis & {
        __TAURI__?: {
          core?: {
            invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
          };
        };
      }
    ).__TAURI__?.core?.invoke;
    if (invoke === undefined) throw new Error("Desktop Tauri bridge is unavailable");
    return await invoke("desktop_continue_task", {
      workspaceId,
      input: { sessionId, taskId, permissionMode: "manual", guidance: null },
    });
  }, decision);
  await browser.refresh();
});

When("I request automatic multi-Agent execution", async () => {
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const reset = await fetch(`${fixtureBaseUrl}/__reset-evidence`, { method: "POST" });
  assert.equal(reset.ok, true, "desktop E2E provider evidence could not be reset");
  await waitForComposerEnabled();
  await setComposerPrompt(desktopProviderCanaries.autoOrchestrationPrompt);
  await submitComposerPrompt();
});

Then("Desktop completes one durable Task from the approved plan", async () => {
  const timeline = await $(".timeline");
  try {
    await timeline.waitUntil(
      async () =>
        (await timeline.getText()).includes(desktopProviderCanaries.autoOrchestrationFinal),
      {
        timeout: 30_000,
        timeoutMsg: "Desktop did not project the approved-plan Task final answer",
      },
    );
  } catch (error) {
    const diagnostic = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const invoke = (
        globalThis as typeof globalThis & {
          __TAURI__?: {
            core?: {
              invoke?: (command: string, input?: Record<string, unknown>) => Promise<unknown>;
            };
          };
        }
      ).__TAURI__?.core?.invoke;
      const call = async (command: string, input?: Record<string, unknown>) => {
        try {
          return await invoke?.(command, input);
        } catch (callError) {
          return { error: String(callError) };
        }
      };
      const workspaceId = panel?.dataset.workspaceId;
      const sessionId = panel?.dataset.sessionId;
      const display = await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = { workspaceId, sessionId, request: { limit: 100 } };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })();
      const taskId = (
        display
        && typeof display === "object"
        && "taskControl" in display
        && display.taskControl
        && typeof display.taskControl === "object"
        && "taskId" in display.taskControl
        && typeof display.taskControl.taskId === "string"
      ) ? display.taskControl.taskId : undefined;
      return {
        body: document.body.innerText,
        panel: {
          workspaceId,
          sessionId,
          lifecycle: panel?.dataset.continuityLifecycle,
          refreshState: panel?.dataset.continuityRefreshState,
          planDebug: panel?.dataset.planDebug,
        },
        display,
        runContext: await call("desktop_run_context", { workspaceId, sessionId }),
        activity: await call("desktop_agent_activity", { workspaceId, sessionId }),
        taskId,
      };
    });
    throw new Error(`Desktop did not project the approved-plan Task final answer: ${JSON.stringify(diagnostic)}`, {
      cause: error,
    });
  }
  await browser.waitUntil(
    async () => {
      const events = durableEvents();
      const directAttempts = durableControls("task_direct_execution_attempt_v1");
      const taskRuns = durableControls("task_run");
      return events.includes("plan_decision_recorded")
        && directAttempts.some((control) => control.status === "completed")
        && taskRuns.some((control) => control.status === "completed");
    },
    {
      timeout: 10_000,
      timeoutMsg: "Desktop approved-plan Task did not persist a completed direct lifecycle",
    },
  );
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const evidence = await fetch(`${fixtureBaseUrl}/__evidence`).then(async (response) => {
    assert.equal(response.ok, true, "desktop E2E provider evidence is unavailable");
    return await response.json() as {
      maxConcurrentReads: number;
      requestCounts: Record<string, number>;
    };
  });
  assert.equal(evidence.maxConcurrentReads, 0, "approved-plan direct execution unexpectedly spawned read Agents");
  assert.equal(evidence.requestCounts.plan_review_request, 1);
  assert.equal(evidence.requestCounts.plan_draft, 1);
  assert.equal(evidence.requestCounts.approved_plan_direct_execution, 1);
  assert.equal(evidence.requestCounts.auto_synthesis ?? 0, 0);
});

When("an unsupported conversation source is stored in the workspace", () => {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const workspaceStateRoot = resolve(runtimeRoot, "state", "workspaces");
  const workspaceState = readdirSync(workspaceStateRoot).find((entry) =>
    statSync(resolve(workspaceStateRoot, entry)).isDirectory());
  assert.ok(workspaceState, "the isolated workspace state directory was not created");
  const sessionDir = resolve(workspaceStateRoot, workspaceState, "sessions");
  mkdirSync(sessionDir, { recursive: true });
  unavailableSessionPath = resolve(sessionDir, unavailableSessionRef);
  writeFileSync(
    unavailableSessionPath,
    `${JSON.stringify({
      user: {
        id: "desktop-e2e-unsupported-message",
        role: "user",
        content: "unsupported conversation source",
        tool_calls: [],
        tool_call_id: null,
      },
    })}\n`,
    "utf8",
  );
});

Then("I can permanently delete the unavailable source from conversation management", async () => {
  assert.ok(unavailableSessionPath, "the unavailable Desktop E2E source was not prepared");

  const topbarActions = await $$(".topbar-actions .sg-icon-button");
  assert.ok(await topbarActions.length >= 2, "the Desktop top bar did not expose conversation management");
  const manageConversations = topbarActions[1];
  assert.ok(manageConversations, "the Desktop conversation management action is unavailable");
  await manageConversations.click();
  await $(".conversation-library-page").waitForDisplayed();

  const row = await $(`//tr[contains(., '${unavailableSessionRef}')]`);
  await row.waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "the unsupported source was not projected into conversation management",
  });
  await row.$('input[type="checkbox"]').click();

  const batchActions = await $$(".library-batch-actions .sg-button");
  assert.equal(batchActions.length, 3, "conversation management did not expose all batch actions");
  const deleteUnavailable = batchActions[2];
  assert.ok(deleteUnavailable, "the unavailable-source delete action is unavailable");
  await deleteUnavailable.waitForEnabled();
  await deleteUnavailable.click();

  const preview = await $('[role="dialog"]');
  await preview.waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "the unavailable source did not receive a batch cleanup preview",
  });
  const applyAction = await preview.$(".confirmation-actions .sg-button-danger");
  await applyAction.waitForEnabled();
  await applyAction.click();

  await browser.waitUntil(
    () => !existsSync(unavailableSessionPath!),
    {
      timeout: 20_000,
      timeoutMsg: "the unavailable source remained on disk after confirmed deletion",
    },
  );
  await $(".batch-receipt").waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "Desktop did not render an unavailable-source deletion receipt",
  });
  await row.waitForExist({
    reverse: true,
    timeout: 20_000,
    timeoutMsg: "the deleted unavailable source remained in conversation management",
  });
});

When("the provider configuration becomes invalid and Desktop restarts", async () => {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  writeFileSync(
    resolve(runtimeRoot, "home", ".sigil", "sigil.toml"),
    "[invalid",
    { encoding: "utf8", mode: 0o600 },
  );
  await browser.refresh();
});

Then("the workspace opens in provider configuration recovery", async () => {
  await $(".app-shell").waitForDisplayed({ timeout: 20_000 });
  const workspace = await $(".workspace-switcher");
  await workspace.waitUntil(
    async () => (await workspace.getText()).includes("desktop-e2e-workspace"),
    {
      timeout: 20_000,
      timeoutMsg: "invalid config prevented the isolated workspace from reopening",
    },
  );
  const body = await $("body");
  await body.waitUntil(
    async () => /Provider configuration is invalid|Provider 配置无效/u.test(await body.getText()),
    {
      timeout: 20_000,
      timeoutMsg: `Desktop did not enter provider configuration recovery:\n${await body.getText()}`,
    },
  );
});

When("I explicitly replace the invalid provider configuration", async () => {
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  await $(".conversation-empty .sg-button-primary").click();
  const replace = await $(".provider-setup-error .sg-button-primary");
  await replace.waitForDisplayed();
  await replace.click();
  await $(".provider-setup-repair").waitForDisplayed();
  const providerChoices = await $$(".provider-choice");
  assert.equal(providerChoices.length, 5, "repair wizard did not expose all provider templates");
  await providerChoices[4]?.click();

  const form = await $(".provider-setup-form");
  const endpoint = await form.$('input:not([type="password"])');
  await endpoint.setValue(fixtureBaseUrl);
  assert.equal(await endpoint.getValue(), fixtureBaseUrl);
  const selects = await form.$$("select");
  assert.equal(await selects.length, 2, "repair form did not expose protocol and authentication");
  const authentication = await selects[1];
  assert.ok(authentication, "repair form authentication control is unavailable");
  await browser.execute((element) => {
    const select = element as HTMLSelectElement;
    select.value = "none";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  }, authentication);
  await authentication.waitUntil(
    async () => (await authentication.getValue()) === "none",
    {
      timeout: 5_000,
      timeoutMsg: "repair form did not select no-authentication",
    },
  );
  const continueToModels = await $(
    ".provider-setup-repair .provider-setup-actions .sg-button-primary",
  );
  await continueToModels.waitForEnabled({
    timeout: 5_000,
    timeoutMsg: "repair form did not enable model discovery",
  });
  await browser.pause(100);
  await $(".provider-setup-repair .provider-setup-actions .sg-button-primary").click();
  const repairSurface = await $(".provider-setup-repair");
  await browser.waitUntil(
    async () =>
      await $(".provider-model-list").isExisting()
      || await $(".provider-setup-repair > p.provider-setup-error").isExisting(),
    {
      timeout: 20_000,
      timeoutMsg: `repair catalog did not reach a terminal UI state:\n${await repairSurface.getText()}`,
    },
  );
  const repairBody = await $("body").getText();
  assert.equal(
    await $(".provider-model-list").isExisting(),
    true,
    `repair catalog did not load:\n${repairBody}`,
  );
  assert.ok(
    await $(
      '.provider-model-list input[type="radio"][value="sigil-e2e-model"]',
    ).isExisting(),
    `repair catalog did not expose the fixture model:\n${repairBody}`,
  );
  await $(".provider-setup-repair .provider-setup-actions .sg-button-primary").click();
  try {
    await $(".provider-connection-list").waitForDisplayed({
      timeout: 20_000,
      timeoutMsg: "the repaired provider inventory did not become visible",
    });
  } catch (error) {
    throw new Error(
      `the repaired provider inventory did not become visible; current UI:\n${await $("body").getText()}`,
      { cause: error },
    );
  }

  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const persisted = readFileSync(
    resolve(runtimeRoot, "home", ".sigil", "sigil.toml"),
    "utf8",
  );
  assert.match(persisted, /config_version = 2/u);
  assert.doesNotMatch(persisted, /\[invalid/u);
});

Then("the repaired workspace can create a new conversation", async () => {
  await $(".application-page-back").click();
  const createConversation = await $(".topbar-actions .sg-icon-button-primary");
  await createConversation.waitForEnabled({
    timeout: 20_000,
    timeoutMsg: "the repaired configuration did not enable new conversations",
  });
});

async function invokeDesktopPlanDecision(
  action: "run" | "save",
): Promise<{ workspaceId: string; sessionId: string; taskId?: string }> {
  return await browser.execute(async (requestedAction) => {
    const panel = document.querySelector<HTMLElement>(".conversation-panel");
    const workspaceId = panel?.dataset.workspaceId;
    const sessionId = panel?.dataset.sessionId;
    const invoke = (
      globalThis as typeof globalThis & {
        __TAURI__?: {
          core?: {
            invoke?: (command: string, input: Record<string, unknown>) => Promise<unknown>;
          };
        };
      }
    ).__TAURI__?.core?.invoke;
    if (invoke === undefined || workspaceId === undefined || sessionId === undefined) {
      throw new Error("Desktop Tauri bridge context is unavailable");
    }
    const display = await (async () => {
          const historyInvoke = invoke;
          if (historyInvoke === undefined) return undefined;
          const historyInput = {
      workspaceId,
      sessionId,
      request: { limit: 50 },
    };
          const binding = { workspaceId: historyInput.workspaceId, sessionId: historyInput.sessionId };
          const queryId = await historyInvoke("desktop_prepare_history_query", binding);
          try {
            return await historyInvoke("desktop_display", { ...historyInput, queryId });
          } finally {
            await historyInvoke("desktop_cancel_history_query", { ...binding, queryId });
          }
        })() as { planReview?: { planId?: string; planHash?: string } };
    const review = display.planReview;
    if (review?.planId === undefined || review.planHash === undefined) {
      throw new Error("Desktop did not expose a complete plan decision binding");
    }
    const decision = await invoke("desktop_plan_decision", {
      workspaceId,
      input: {
        sessionId,
        planId: review.planId,
        expectedPlanHash: review.planHash,
        action: requestedAction,
      },
    }) as { taskId?: string };
    return { workspaceId, sessionId, taskId: decision.taskId };
  }, action);
}

function durableEvents(): string[] {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const events: string[] = [];
  for (const path of filesUnder(runtimeRoot)) {
    if (!path.endsWith(".jsonl")) continue;
    for (const line of readFileSync(path, "utf8").split("\n")) {
      if (line.trim() === "") continue;
      const record = JSON.parse(line) as {
        event_type?: unknown;
        payload?: {
          session_log_entry?: {
            control?: unknown;
          };
        };
      };
      if (typeof record.event_type === "string") events.push(record.event_type);
      const control = record.payload?.session_log_entry?.control;
      if (control !== null && typeof control === "object" && !Array.isArray(control)) {
        events.push(...Object.keys(control));
      }
    }
  }
  return events;
}

function durableEvidenceContains(fragment: string): boolean {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  return filesUnder(runtimeRoot).some((path) =>
    path.endsWith(".jsonl") && readFileSync(path, "utf8").includes(fragment),
  );
}

function durableControls(key: string): Array<Record<string, unknown>> {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const controls: Array<Record<string, unknown>> = [];
  for (const path of filesUnder(runtimeRoot)) {
    if (!path.endsWith(".jsonl")) continue;
    for (const line of readFileSync(path, "utf8").split("\n")) {
      if (line.trim() === "") continue;
      const record = JSON.parse(line) as {
        payload?: {
          session_log_entry?: {
            control?: Record<string, unknown>;
          };
        };
      };
      const value = record.payload?.session_log_entry?.control?.[key];
      if (value !== null && typeof value === "object" && !Array.isArray(value)) {
        controls.push(value as Record<string, unknown>);
      }
    }
  }
  return controls;
}

function matchingUserInputControls(key: string): Array<Record<string, unknown>> {
  return durableControls(key).filter((control) => {
    if (key === "user_input_requested") {
      const request = control.request;
      return request !== null
        && typeof request === "object"
        && !Array.isArray(request)
        && (request as Record<string, unknown>).prompt
          === desktopProviderCanaries.durableUserInputCardPrompt;
    }
    if (durableUserInputIdentity === undefined) return false;
    const identity = control.identity;
    return identity !== null
      && typeof identity === "object"
      && !Array.isArray(identity)
      && (identity as Record<string, unknown>).request_id === durableUserInputIdentity.requestId
      && (identity as Record<string, unknown>).generation === durableUserInputIdentity.generation
      && control.request_hash === durableUserInputIdentity.requestHash;
  });
}

function requestedIdentity(control: Record<string, unknown>): DurableUserInputIdentity {
  const request = control.request;
  assert.ok(request !== null && typeof request === "object" && !Array.isArray(request));
  const identity = (request as Record<string, unknown>).identity;
  assert.ok(identity !== null && typeof identity === "object" && !Array.isArray(identity));
  const requestId = (identity as Record<string, unknown>).request_id;
  const generation = (identity as Record<string, unknown>).generation;
  const requestHash = control.request_hash;
  assert.ok(typeof requestId === "string");
  assert.ok(typeof generation === "number");
  assert.ok(typeof requestHash === "string");
  return { requestId, generation, requestHash };
}

async function providerEvidence(): Promise<{
  maxConcurrentReads: number;
  requestCounts: Record<string, number>;
}> {
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const response = await fetch(`${fixtureBaseUrl}/__evidence`);
  assert.equal(response.ok, true, "desktop E2E provider evidence is unavailable");
  return await response.json() as {
    maxConcurrentReads: number;
    requestCounts: Record<string, number>;
  };
}

function filesUnder(root: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(root)) {
    const path = resolve(root, entry);
    if (statSync(path).isDirectory()) files.push(...filesUnder(path));
    else files.push(path);
  }
  return files;
}


let directLifecycleBinding: { workspaceId: string; sessionId: string; taskId: string; admissionId: string };

async function directFixtureCalls(): Promise<number> {
  const response = await fetch(`${process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL}/__evidence`);
  assert.ok(response.ok);
  const evidence = await response.json() as { requestCounts: Record<string, number> };
  return evidence.requestCounts.approved_plan_direct_execution ?? 0;
}

When("I hold the next Direct Task response", async () => {
  const response = await fetch(`${process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL}/__hold-next-direct`, { method: "POST" });
  assert.ok(response.ok);
});

When("I pause the Direct Task and reload Desktop", async () => {
  await browser.waitUntil(async () => await directFixtureCalls() === 1, { timeout: 20_000 });
  const admission = durableControls("task_direct_execution_admitted_v1").find((entry) => entry.task_id === directLifecycleBinding.taskId);
  assert.ok(admission);
  const panel = await $(".conversation-panel");
  directLifecycleBinding = {
    workspaceId: await panel.getAttribute("data-workspace-id"),
    sessionId: await panel.getAttribute("data-session-id"),
    taskId: String(admission.task_id),
    admissionId: String(admission.admission_id),
  };
  const runId = await panel.getAttribute("data-continuity-owner-run-id");
  assert.ok(runId, "native continuity must retain the active Direct run");
  await browser.execute(async ({ binding, runId }) => {
    const invoke = (globalThis as typeof globalThis & { __TAURI__: { core: { invoke: (command: string, input: unknown) => Promise<unknown> } } }).__TAURI__.core.invoke;
    await invoke("desktop_pause_task", { workspaceId: binding.workspaceId, input: {
      sessionId: binding.sessionId, runId, taskId: binding.taskId,
      execution: { kind: "direct", admissionId: binding.admissionId },
    } });
  }, { binding: directLifecycleBinding, runId });
  await browser.waitUntil(async () => durableControls("task_run").some((entry) => entry.task_id === directLifecycleBinding.taskId && entry.status === "paused"), { timeout: 15_000 });
  await browser.refresh();
  await $(".conversation-panel").waitForDisplayed({ timeout: 15_000 });
  assert.equal(await directFixtureCalls(), 1, "reload must not execute the paused Task");
});

When("I continue the paused Direct Task twice", async () => {
  const results = await browser.execute(async (binding) => {
    const invoke = (globalThis as typeof globalThis & { __TAURI__: { core: { invoke: (command: string, input: unknown) => Promise<unknown> } } }).__TAURI__.core.invoke;
    const input = { workspaceId: binding.workspaceId, input: { sessionId: binding.sessionId, taskId: binding.taskId, permissionMode: "manual", guidance: null } };
    return await Promise.all([0, 1].map(async () => {
      try { await invoke("desktop_continue_task", input); return "accepted"; }
      catch { return "rejected"; }
    }));
  }, directLifecycleBinding);
  assert.equal(results.filter((result) => result === "accepted").length, 1, "one native continuation owns the resumed attempt");
});

Then("one Direct Task completes and survives reload without reexecution", async () => {
  const taskId = directLifecycleBinding.taskId;
  await browser.waitUntil(async () => durableControls("task_run").some((entry) => entry.task_id === taskId && entry.status === "completed"), { timeout: 30_000 });
  const attempts = () => durableControls("task_direct_execution_attempt_v1").filter((entry) => entry.task_id === taskId);
  assert.equal(new Set(attempts().map((entry) => entry.attempt_id)).size, 2, "pause and continue retain exactly two attempts");
  assert.equal(new Set(durableControls("task_direct_execution_admitted_v1").filter((entry) => entry.task_id === taskId).map((entry) => entry.admission_id)).size, 1);
  assert.equal(durableControls("task_plan").filter((entry) => entry.task_id === taskId).length, 0);
  assert.equal(await directFixtureCalls(), 2);
  await browser.refresh();
  await browser.waitUntil(async () => (await $(".timeline").getText()).includes(desktopProviderCanaries.autoOrchestrationFinal), { timeout: 15_000 });
  const rejection = await browser.execute(async (binding) => {
    type RunEvent = { kind: string; runId: string; sessionId: string };
    const tauri = (globalThis as typeof globalThis & { __TAURI__: {
      core: { invoke: (command: string, input: unknown) => Promise<{ id: string }> };
      event: { listen: (name: string, handler: (event: { payload: RunEvent }) => void) => Promise<() => void> };
    } }).__TAURI__;
    const observed: RunEvent[] = [];
    let receive: ((event: RunEvent) => void) | undefined;
    const unlisten = await tauri.event.listen("sigil-run-event", ({ payload }) => {
      observed.push(payload);
      receive?.(payload);
    });
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      // Run admission precedes asynchronous Task validation. Observe the exact admitted
      // run's terminal event instead of treating an IPC receipt as Task execution success.
      const run = await tauri.core.invoke("desktop_continue_task", { workspaceId: binding.workspaceId, input: { sessionId: binding.sessionId, taskId: binding.taskId, permissionMode: "manual", guidance: null } });
      const matches = (event: RunEvent) => event.runId === run.id
        && event.sessionId === binding.sessionId && event.kind === "run_failed";
      const event = observed.find(matches) ?? await new Promise<RunEvent>((resolve, reject) => {
        receive = (next) => { if (matches(next)) resolve(next); };
        timer = setTimeout(() => reject(new Error("stale continuation did not reach run_failed")), 15_000);
      });
      return { runId: run.id, kind: event.kind };
    } finally {
      if (timer !== undefined) clearTimeout(timer);
      unlisten();
    }
  }, directLifecycleBinding);
  assert.ok(rejection.runId);
  assert.equal(rejection.kind, "run_failed", "stale continuation must reject the completed Task before execution");
  assert.equal(durableControls("task_run").filter((entry) => entry.task_id === taskId).at(-1)?.status, "completed");
  assert.equal(await directFixtureCalls(), 2);
  assert.equal(new Set(attempts().map((entry) => entry.attempt_id)).size, 2);
});
