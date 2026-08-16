import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";

import { Given, Then, When } from "@wdio/cucumber-framework";
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

async function waitForComposerEnabled(
  timeoutMsg = "the desktop composer did not become enabled",
  timeout = 20_000,
): Promise<void> {
  await browser.waitUntil(
    async () => {
      try {
        const composerEnabled = await $("#desktop-prompt").isEnabled();
        const lifecycle = await $(".conversation-panel").getAttribute(
          "data-continuity-lifecycle",
        );
        return composerEnabled && (lifecycle === "idle" || lifecycle === "live");
      } catch {
        // Canonical refreshes may replace the textarea while WebDriver is observing it. Re-query
        // on every attempt instead of retaining a stale native WebKit element reference.
        return false;
      }
    },
    { timeout, timeoutMsg },
  );
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
  await createConversation.waitForEnabled({
    timeout: 20_000,
    timeoutMsg: "the configured local provider did not enable new conversations",
  });
  await createConversation.click();
  await browser.waitUntil(
    async () => {
      try {
        return await $("#desktop-prompt").isEnabled();
      } catch {
        return false;
      }
    },
    {
      timeout: 20_000,
      timeoutMsg: "the fresh conversation composer was not enabled",
    },
  );
  await browser.waitUntil(
    async () => {
      try {
        return (await $("#desktop-prompt").getValue()) === "";
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
  const composer = await $("#desktop-prompt");
  await composer.setValue("请验证 Desktop 在审批等待期间仍可排队消息，并完成恢复。");
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
  const composer = await $("#desktop-prompt");
  await composer.click();
  await composer.setValue(desktopProviderCanaries.queuedPrompt);
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
  assert.equal(await $("#desktop-prompt").getValue(), "");
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
      if (actions.length !== 0) return false;
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
        serverDisplay = await tauri?.core?.invoke?.("desktop_display", {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
          request: { limit: 50 },
        });
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
  const composer = await $("#desktop-prompt");
  await composer.setValue("$desktop-e2e-skill inspect README");
  await browser.keys("Enter");
});

Then("the custom workspace skill executes with durable load evidence", async () => {
  const timeline = await $(".timeline");
  try {
    await timeline.waitUntil(
      async () => (await timeline.getText()).includes(desktopProviderCanaries.skillRun),
      {
        timeout: 30_000,
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
  const composer = await $("#desktop-prompt");
  await composer.setValue("@desktop-e2e-agent inspect README");
  await browser.keys("Enter");
});

Then("the custom workspace agent executes with its profile instructions", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.agentRun),
    {
      timeout: 30_000,
      timeoutMsg: "the custom workspace agent did not execute its profile instructions",
    },
  );
});

When("I start a persistent terminal task from Desktop", async () => {
  await waitForComposerEnabled();
  const composer = await $("#desktop-prompt");
  await composer.setValue(desktopProviderCanaries.terminalLifecyclePrompt);
  await browser.keys("Enter");

  await browser.waitUntil(
    async () => {
      const approval = await $(".approval-dock");
      const card = await $(".terminal-task-card");
      return await approval.isExisting() || await card.isExisting();
    },
    {
      timeout: 30_000,
      timeoutMsg: "the persistent terminal request produced neither approval nor a task card",
    },
  );
  const approval = await $(".approval-dock");
  if (await approval.isExisting()) {
    await $(".approval-actions .sg-button-primary").click();
  }
});

Then("the terminal task becomes ready before the foreground answer completes", async () => {
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  await card.waitForDisplayed({
    timeout: 30_000,
    timeoutMsg: "Desktop did not render the persistent terminal task card",
  });
  await browser.waitUntil(
    async () => (
      await card.getAttribute("data-terminal-task-readiness") === "ready"
      && await card.getAttribute("data-terminal-task-status") === "running"
    ),
    {
      timeout: 20_000,
      timeoutMsg: "the terminal task did not reach ready/running",
    },
  );
  assert.ok(
    (await card.getText()).includes(desktopProviderCanaries.terminalLifecycleReady) === false,
    "the bounded card leaked raw terminal output instead of lifecycle facts",
  );
});

Then("the foreground answer completes while the terminal task remains running", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.terminalLifecycleFinal),
    {
      timeout: 30_000,
      timeoutMsg: "the foreground answer did not complete after terminal readiness",
    },
  );
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  assert.equal(await card.getAttribute("data-terminal-task-status"), "running");
});

When("I start a successor run while the older terminal task remains running", async () => {
  await waitForComposerEnabled("the composer did not recover after foreground completion");
  const composer = await $("#desktop-prompt");
  await composer.setValue(desktopProviderCanaries.terminalSuccessorPrompt);
  await browser.keys("Enter");
});

Then("the successor completes while the older terminal task is still tracked", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.terminalSuccessorFinal),
    {
      timeout: 20_000,
      timeoutMsg: "the successor run did not complete while the older terminal owner was retained",
    },
  );
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  assert.equal(
    await card.getAttribute("data-terminal-task-status"),
    "running",
    "the successor run displaced the older terminal owner before it settled",
  );
});

When("I stop the retained terminal task", async () => {
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  const stop = await card.$("button");
  await stop.waitForEnabled({
    timeout: 10_000,
    timeoutMsg: "the retained terminal task did not expose its stop control",
  });
  await stop.click();
});

Then("the retained terminal task becomes cancelled", async () => {
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  await browser.waitUntil(
    async () => await card.getAttribute("data-terminal-task-status") === "cancelled",
    {
      timeout: 20_000,
      timeoutMsg: "the retained terminal task did not reach cancelled",
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
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  await card.waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "continuity did not restore the retained terminal task card",
  });
  const status = await card.getAttribute("data-terminal-task-status");
  assert.ok(
    status === "running" || status === "exited",
    `continuity restored an invalid terminal status: ${status}`,
  );
  const generation = Number.parseInt(
    await card.getAttribute("data-terminal-task-generation") ?? "0",
    10,
  );
  assert.ok(generation > 0, "continuity did not restore a generation-bound terminal owner");
  assert.equal(
    await $(".conversation-continuity-loading").isExisting(),
    false,
    "continuity hydration left the conversation in recovery",
  );
});

When("an Agent asks a durable question from Desktop", async () => {
  durableUserInputIdentity = undefined;
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const reset = await fetch(`${fixtureBaseUrl}/__reset-evidence`, { method: "POST" });
  assert.equal(reset.ok, true, "desktop E2E provider evidence could not be reset");

  await waitForComposerEnabled();
  const composer = await $("#desktop-prompt");
  await composer.setValue(desktopProviderCanaries.durableUserInputPrompt);
  await browser.keys("Enter");
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
      let display: unknown;
      try {
        display = await invoke?.("desktop_display", {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
          request: { limit: 50 },
        });
      } catch (displayError) {
        display = { error: String(displayError) };
      }
      return {
        body: document.body.textContent,
        continuityLifecycle: panel?.dataset.continuityLifecycle,
        continuityRefreshState: panel?.dataset.continuityRefreshState,
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
  const card = await $(".terminal-task-card[data-terminal-task-id='desktop-e2e-terminal-task']");
  await browser.waitUntil(
    async () => await card.getAttribute("data-terminal-task-status") === "exited",
    {
      timeout: 20_000,
      timeoutMsg: "Desktop stopped following terminal lifecycle after foreground completion",
    },
  );
  const generation = Number.parseInt(
    await card.getAttribute("data-terminal-task-generation") ?? "0",
    10,
  );
  assert.ok(generation > 1, `the terminal lifecycle generation did not advance: ${generation}`);
  assert.ok((await card.getText()).includes("0"), "the exited task card did not show its exit code");
  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  assert.ok(artifactRoot, "desktop E2E artifact directory is configured");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-terminal-lifecycle.png"));
});

When("I invoke Desktop plan mode", async () => {
  await waitForComposerEnabled();
  const composer = await $("#desktop-prompt");
  await composer.setValue("/plan inspect the runtime architecture");
  await browser.keys("Enter");
});

Then("the supervised plan agent drafts a durable plan", async () => {
  await browser.waitUntil(
    async () => durableEvidenceContains('"source":"explicit_plan_command"'),
    {
      timeout: 10_000,
      timeoutMsg: "Desktop plan mode did not persist an explicit plan review attempt",
    },
  );
});

Then("the automatic plan review drafts a durable plan", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.planDraftSummary),
    {
      timeout: 30_000,
      timeoutMsg: "Desktop automatic plan review did not commit a draft plan",
    },
  );
});

Then("the draft plan becomes ready on the Desktop plan card", async () => {
  const card = await $(".plan-card");
  await card.waitForDisplayed({ timeout: 15_000 });
  await browser.waitUntil(
    async () => (await card.getText()).includes(desktopProviderCanaries.planDraftSummary),
    {
      timeout: 10_000,
      timeoutMsg: "Desktop did not render the ready draft plan card",
    },
  );
});

When("I save the reviewed plan from Desktop", async () => {
  const card = await $(".plan-card");
  await card.$("[data-plan-detail-toggle]").click();
  const saveButton = await card.$("[data-plan-action='save']");
  await saveButton.waitForClickable({ timeout: 10_000 });
  await saveButton.click();
});

Then("the plan card closes without creating a Task", async () => {
  await $(".plan-card").waitForDisplayed({ timeout: 10_000, reverse: true });
  await browser.waitUntil(
    async () => !durableControls("task_step").some((control) => control.status === "started"),
    {
      timeout: 10_000,
      timeoutMsg: "Saving the reviewed plan unexpectedly created a Task",
    },
  );
});

When("I run the reviewed plan from Desktop", async () => {
  const card = await $(".plan-card");
  await card.$("[data-plan-detail-toggle]").click();
  const runButton = await card.$("[data-plan-action='run']");
  try {
    await browser.waitUntil(async () => runButton.isClickable(), {
      timeout: 15_000,
      timeoutMsg: "Desktop plan remained non-runnable",
    });
  } catch (error) {
    const diagnostic = await browser.execute(async () => {
      const panel = document.querySelector<HTMLElement>(".conversation-panel");
      const button = document.querySelector<HTMLButtonElement>("[data-plan-action='run']");
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
      try {
        display = await invoke?.("desktop_display", {
          workspaceId: panel?.dataset.workspaceId,
          sessionId: panel?.dataset.sessionId,
          request: { limit: 50 },
        });
      } catch (displayError) {
        display = { error: String(displayError) };
      }
      return {
        card: document.querySelector(".plan-card")?.textContent,
        buttonDisabled: button?.disabled,
        continuityLifecycle: panel?.dataset.continuityLifecycle,
        continuityRefreshState: panel?.dataset.continuityRefreshState,
        continuityPendingTerminalRunId: panel?.dataset.continuityPendingTerminalRunId,
        display,
      };
    });
    throw new Error(`Desktop plan remained non-runnable: ${JSON.stringify(diagnostic)}`, {
      cause: error,
    });
  }
  await runButton.click();
});

When("I request automatic multi-Agent execution", async () => {
  const fixtureBaseUrl = process.env.SIGIL_DESKTOP_E2E_PROVIDER_BASE_URL;
  assert.ok(fixtureBaseUrl, "desktop E2E provider fixture URL is configured");
  const reset = await fetch(`${fixtureBaseUrl}/__reset-evidence`, { method: "POST" });
  assert.equal(reset.ok, true, "desktop E2E provider evidence could not be reset");
  await waitForComposerEnabled();
  const composer = await $("#desktop-prompt");
  await composer.setValue(desktopProviderCanaries.autoOrchestrationPrompt);
  await browser.keys("Enter");
});

Then("Desktop completes one durable task with two overlapping read Agents", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () =>
      (await timeline.getText()).includes(desktopProviderCanaries.autoOrchestrationFinal),
    {
      timeout: 30_000,
      timeoutMsg: "Desktop did not project the automatic orchestration final answer",
    },
  );
  await browser.waitUntil(
    async () => {
      const events = durableEvents();
      const completedSteps = durableControls("task_step").filter(
        (control) => control.status === "completed",
      );
      return completedSteps.length >= 2
        && events.includes("task_plan")
        && events.includes("plan_decision_recorded");
    },
    {
      timeout: 10_000,
      timeoutMsg: "Desktop automatic orchestration did not persist the task lifecycle",
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
  assert.equal(evidence.maxConcurrentReads, 2, "Desktop read Agents did not overlap");
  assert.equal(evidence.requestCounts.plan_review_request, 1);
  assert.equal(evidence.requestCounts.plan_draft, 1);
  assert.equal(evidence.requestCounts.auto_synthesis, 1);
  for (const stepId of desktopProviderCanaries.autoReadStepIds) {
    assert.equal(evidence.requestCounts[`auto_read:${stepId}`], 1);
  }
});

When("an unsupported conversation source is stored in the workspace", () => {
  const runtimeRoot = process.env.SIGIL_DESKTOP_E2E_ROOT;
  assert.ok(runtimeRoot, "desktop E2E runtime root is configured");
  const currentSession = filesUnder(runtimeRoot).find((path) =>
    path.endsWith(".jsonl") && basename(dirname(path)) === "sessions",
  );
  assert.ok(currentSession, "the source-built Desktop did not create a durable session directory");

  unavailableSessionPath = resolve(dirname(currentSession), unavailableSessionRef);
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
  assert.ok(topbarActions.length >= 2, "the Desktop top bar did not expose conversation management");
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
  assert.equal(selects.length, 2, "repair form did not expose protocol and authentication");
  const authentication = selects.at(-1);
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
  await $(".provider-connection-list").waitForDisplayed({
    timeout: 20_000,
    timeoutMsg: "the repaired provider inventory did not become visible",
  });

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
  assert.equal(typeof requestId, "string");
  assert.equal(typeof generation, "number");
  assert.equal(typeof requestHash, "string");
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
