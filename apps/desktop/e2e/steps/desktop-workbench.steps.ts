import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";

import { Given, Then, When } from "@wdio/cucumber-framework";
import { $, $$, browser } from "@wdio/globals";

import { desktopProviderCanaries } from "../provider-fixture";

const unavailableSessionRef = "desktop-e2e-unsupported.jsonl";
let unavailableSessionPath: string | undefined;

Given("the current-source desktop has restored the isolated workspace", async () => {
  await $(".app-shell").waitForDisplayed();
  const workspace = await $(".workspace-switcher");
  await workspace.waitUntil(
    async () => (await workspace.getText()).includes("desktop-e2e-workspace"),
    { timeout: 20_000, timeoutMsg: "the isolated workspace was not restored" },
  );
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
  await $("#desktop-prompt").waitForEnabled();

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
  const composer = await $("#desktop-prompt");
  await composer.setValue("请验证 Desktop 在审批等待期间仍可排队消息，并完成恢复。");
  await browser.keys("Enter");
  await $(".approval-dock").waitForDisplayed({
    timeout: 30_000,
    timeoutMsg: "the real runtime did not surface its approval request",
  });
});

Then("the approval remains live without losing runtime control", async () => {
  await browser.pause(12_000);
  await $(".approval-dock").waitForDisplayed();
  assert.equal(
    await $$(".notification-toast").some(async (toast) =>
      (await toast.getText()).includes("无法连接实时运行控制")
      || (await toast.getText()).includes("Unable to connect to live runtime controls")),
    false,
    "live runtime control entered an error state while approval was pending",
  );
  await $("#desktop-prompt").waitForEnabled();

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

Then("the initial run and queued follow-up both complete", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.initialRun),
    {
      timeout: 30_000,
      timeoutMsg: "the approved run did not complete",
    },
  );
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.queuedRun),
    {
      timeout: 30_000,
      timeoutMsg: "the durable follow-up did not dispatch after the active run",
    },
  );

  const artifactRoot = process.env.SIGIL_DESKTOP_E2E_ARTIFACTS;
  assert.ok(artifactRoot, "desktop E2E artifact directory is configured");
  await browser.saveScreenshot(resolve(artifactRoot, "desktop-real-run.png"));
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
  const composer = await $("#desktop-prompt");
  await composer.waitForEnabled({ timeout: 20_000 });
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
  const composer = await $("#desktop-prompt");
  await composer.waitForEnabled({ timeout: 20_000 });
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

When("I invoke Desktop plan mode", async () => {
  const composer = await $("#desktop-prompt");
  await composer.waitForEnabled({ timeout: 20_000 });
  await composer.setValue("/plan inspect the runtime architecture");
  await browser.keys("Enter");
});

Then("the supervised plan agent executes with durable profile evidence", async () => {
  const timeline = await $(".timeline");
  await timeline.waitUntil(
    async () => (await timeline.getText()).includes(desktopProviderCanaries.planRun),
    {
      timeout: 30_000,
      timeoutMsg: "Desktop plan mode did not execute the supervised plan agent",
    },
  );
  await browser.waitUntil(
    async () => durableEvidenceContains('"profile_id":"plan"'),
    {
      timeout: 10_000,
      timeoutMsg: "Desktop plan mode did not persist the plan profile binding",
    },
  );
});

When("I request automatic multi-Agent execution", async () => {
  const composer = await $("#desktop-prompt");
  await composer.waitForEnabled({ timeout: 20_000 });
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
        && events.includes("task_handoff_requested")
        && events.includes("task_plan");
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
  assert.equal(evidence.requestCounts.auto_conversation, 1);
  assert.equal(evidence.requestCounts.auto_planner, 1);
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

function filesUnder(root: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(root)) {
    const path = resolve(root, entry);
    if (statSync(path).isDirectory()) files.push(...filesUnder(path));
    else files.push(path);
  }
  return files;
}
