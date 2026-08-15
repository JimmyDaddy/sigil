import { createServer, type IncomingMessage, type ServerResponse } from "node:http";

const TITLE_CANARY = "验证桌面审批队列与恢复";
const INITIAL_RUN_CANARY = "DESKTOP_E2E_INITIAL_DONE";
const QUEUED_RUN_CANARY = "DESKTOP_E2E_QUEUED_DONE";
const QUEUED_PROMPT = "继续验证排队消息";
const APPROVAL_CALL_ID = "desktop-e2e-approval-call";
const SKILL_INSTRUCTION_MARKER = "DESKTOP_E2E_SKILL_INSTRUCTION";
const AGENT_INSTRUCTION_MARKER = "DESKTOP_E2E_AGENT_INSTRUCTION";
const PLAN_INSTRUCTION_MARKER = "Research and plan before execution; do not edit files directly.";
const SKILL_RUN_CANARY = "DESKTOP_E2E_SKILL_DONE";
const AGENT_RUN_CANARY = "DESKTOP_E2E_AGENT_DONE";
const PLAN_RUN_CANARY = "DESKTOP_E2E_PLAN_DONE";
const AUTO_ORCHESTRATION_PROMPT = "DESKTOP_E2E_AUTO_ORCHESTRATION";
const AUTO_ORCHESTRATION_FINAL_CANARY = "DESKTOP_E2E_AUTO_ORCHESTRATION_DONE";
const TERMINAL_LIFECYCLE_PROMPT = "DESKTOP_E2E_TERMINAL_LIFECYCLE";
const TERMINAL_LIFECYCLE_READY_CANARY = "DESKTOP_E2E_TERMINAL_READY";
const TERMINAL_LIFECYCLE_FINAL_CANARY = "DESKTOP_E2E_TERMINAL_FOREGROUND_DONE";
const TERMINAL_SUCCESSOR_PROMPT = "DESKTOP_E2E_TERMINAL_SUCCESSOR";
const TERMINAL_SUCCESSOR_FINAL_CANARY = "DESKTOP_E2E_TERMINAL_SUCCESSOR_DONE";
const AUTO_READ_STEP_IDS = ["desktop_inspect_kernel", "desktop_inspect_runtime"] as const;
const AUTO_HANDOFF_ARGS = JSON.stringify({
  reason_codes: ["parallel_research", "multi_stage_change"],
});
const AUTO_PLAN_ARGS = JSON.stringify({
  plan_version: 1,
  status: "accepted",
  steps: AUTO_READ_STEP_IDS.map((stepId) => ({
    step_id: stepId,
    title: `Inspect ${stepId.replace("desktop_inspect_", "")}`,
    role: "subagent_read",
    mode: "read",
    isolation: "shared_read_only",
  })),
});
const PLAN_REVIEW_REQUEST_ARGS = JSON.stringify({
  reason_codes: ["explicit_review_intent", "architectural_tradeoff"],
});
const PLAN_REVIEW_DRAFT_SUMMARY = "DESKTOP_E2E_PLAN_DRAFT";
const planReviewDraftArgs = (summary: string) => JSON.stringify({
  schema_version: 2,
  summary,
  steps: AUTO_READ_STEP_IDS.map((stepId) => ({
    step_id: stepId,
    title: `Inspect ${stepId.replace("desktop_inspect_", "")}`,
    role: "subagent_read",
    mode: "read",
    isolation: "shared_read_only",
  })),
  target_paths: [],
  suggested_checks: [],
});

interface ChatMessage {
  readonly role?: string;
  readonly content?: unknown;
}

interface ChatCompletionRequest {
  readonly messages?: ChatMessage[];
  readonly tools?: unknown[];
}

interface FixtureEvidence {
  readonly maxConcurrentReads: number;
  readonly requestCounts: Readonly<Record<string, number>>;
}

export interface DesktopProviderFixture {
  readonly baseUrl: string;
}

export async function startDesktopProviderFixture(): Promise<DesktopProviderFixture> {
  const requestCounts = new Map<string, number>();
  let concurrentReads = 0;
  let maxConcurrentReads = 0;
  const recordRequest = (kind: string) => {
    requestCounts.set(kind, (requestCounts.get(kind) ?? 0) + 1);
  };
  const server = createServer(async (request, response) => {
    try {
      if (request.method === "GET") {
        if (request.url?.endsWith("/__evidence")) {
          sendJson(response, {
            maxConcurrentReads,
            requestCounts: Object.fromEntries(requestCounts),
          } satisfies FixtureEvidence);
          return;
        }
        sendJson(response, {
          object: "list",
          data: [{ id: "sigil-e2e-model", object: "model" }],
        });
        return;
      }
      if (request.method === "POST" && request.url?.endsWith("/__reset-evidence")) {
        requestCounts.clear();
        concurrentReads = 0;
        maxConcurrentReads = 0;
        sendJson(response, { reset: true });
        return;
      }
      if (request.method !== "POST" || !request.url?.endsWith("/chat/completions")) {
        sendJson(response, { error: "unexpected desktop E2E provider request" }, 404);
        return;
      }

      const payload = JSON.parse(await readRequestBody(request)) as ChatCompletionRequest;
      const messages = payload.messages ?? [];
      const lastMessage = messages.at(-1);
      const requestText = messages
        .map((message) => typeof message.content === "string" ? message.content : "")
        .join("\n");
      const lastUserText = [...messages]
        .reverse()
        .find((message) => message.role === "user")
        ?.content;
      const toolNames = new Set(
        (payload.tools ?? []).flatMap((tool) => {
          if (tool === null || typeof tool !== "object") return [];
          const candidate = tool as { function?: { name?: unknown } };
          return typeof candidate.function?.name === "string"
            ? [candidate.function.name]
            : [];
        }),
      );

      if (
        (payload.tools?.length ?? 0) === 0
        && requestText.includes(
          "Generate a concise semantic title for a coding-agent conversation",
        )
      ) {
        recordRequest("title");
        sendText(response, TITLE_CANARY);
      } else if (
        toolNames.has("request_plan_review")
        && typeof lastUserText === "string"
        && lastUserText.includes(AUTO_ORCHESTRATION_PROMPT)
      ) {
        recordRequest("plan_review_request");
        sendNamedToolCall(
          response,
          "desktop-auto-plan-review",
          "request_plan_review",
          PLAN_REVIEW_REQUEST_ARGS,
        );
      } else if (toolNames.has("submit_plan_draft")) {
        recordRequest("plan_draft");
        sendNamedToolCall(
          response,
          "desktop-auto-plan-draft",
          "submit_plan_draft",
          planReviewDraftArgs(PLAN_REVIEW_DRAFT_SUMMARY),
        );
      } else if (
        toolNames.has("request_task_planning")
        && typeof lastUserText === "string"
        && lastUserText.includes(AUTO_ORCHESTRATION_PROMPT)
      ) {
        recordRequest("auto_conversation");
        sendNamedToolCall(
          response,
          "desktop-auto-handoff",
          "request_task_planning",
          AUTO_HANDOFF_ARGS,
        );
      } else if (toolNames.has("continue_without_task_planning")) {
        recordRequest("direct_conversation_routing");
        sendNamedToolCall(
          response,
          `desktop-direct-conversation-${requestCounts.get("direct_conversation_routing")}`,
          "continue_without_task_planning",
          JSON.stringify({
            reason: "does_not_meet_task_planning_criteria",
          }),
        );
      } else if (toolNames.has("task_plan_update")) {
        recordRequest("auto_planner");
        sendNamedToolCall(
          response,
          "desktop-auto-plan",
          "task_plan_update",
          AUTO_PLAN_ARGS,
        );
      } else if (requestText.includes("Produce the single user-visible final answer")) {
        // Synthesis includes the approved plan and therefore also contains the read-role labels.
        // Match the dedicated synthesis instruction before the participant-step branch.
        recordRequest("auto_synthesis");
        sendText(response, AUTO_ORCHESTRATION_FINAL_CANARY);
      } else if (requestText.includes("Role: subagent_read")) {
        const stepId = AUTO_READ_STEP_IDS.find((candidate) =>
          requestText.includes(`Step: ${candidate}`),
        );
        if (stepId === undefined) {
          throw new Error("Desktop orchestration read request did not bind a known step");
        }
        recordRequest(`auto_read:${stepId}`);
        concurrentReads += 1;
        maxConcurrentReads = Math.max(maxConcurrentReads, concurrentReads);
        await new Promise((resolve) => setTimeout(resolve, 350));
        concurrentReads -= 1;
        sendText(response, `bounded result for ${stepId}`);
      } else if (requestText.includes(SKILL_INSTRUCTION_MARKER)) {
        recordRequest("workspace_skill");
        sendText(response, SKILL_RUN_CANARY);
      } else if (requestText.includes(AGENT_INSTRUCTION_MARKER)) {
        recordRequest("workspace_agent");
        sendText(response, AGENT_RUN_CANARY);
      } else if (requestText.includes(PLAN_INSTRUCTION_MARKER)) {
        recordRequest("plan_agent");
        sendText(response, PLAN_RUN_CANARY);
      } else if (typeof lastUserText === "string" && lastUserText.includes(QUEUED_PROMPT)) {
        recordRequest("queued_followup");
        sendText(response, QUEUED_RUN_CANARY);
      } else if (
        typeof lastUserText === "string"
        && lastUserText.includes(TERMINAL_SUCCESSOR_PROMPT)
      ) {
        recordRequest("terminal_successor");
        sendText(response, TERMINAL_SUCCESSOR_FINAL_CANARY);
      } else if (
        lastMessage?.role === "tool"
        && typeof lastMessage.content === "string"
        && lastMessage.content.includes("ordinary conversation routing accepted")
        && requestText.includes(TERMINAL_LIFECYCLE_PROMPT)
      ) {
        recordRequest("terminal_lifecycle_initial");
        sendNamedToolCall(
          response,
          "desktop-e2e-terminal-start",
          "terminal_start",
          JSON.stringify({
            task_id: "desktop-e2e-terminal-task",
            command: `printf '${TERMINAL_LIFECYCLE_READY_CANARY}\\n'; sleep 12; printf 'DESKTOP_E2E_TERMINAL_EXIT\\n'`,
            mode: "background",
            readiness: {
              kind: "output_contains",
              value: TERMINAL_LIFECYCLE_READY_CANARY,
              timeout_secs: 5,
            },
          }),
        );
      } else if (lastMessage?.role === "tool" && requestText.includes(TERMINAL_LIFECYCLE_PROMPT)) {
        recordRequest("terminal_lifecycle_after_start");
        sendText(response, TERMINAL_LIFECYCLE_FINAL_CANARY);
      } else if (
        lastMessage?.role === "tool"
        && typeof lastMessage.content === "string"
        && lastMessage.content.includes("ordinary conversation routing accepted")
      ) {
        recordRequest("approval_initial");
        sendToolCall(response);
      } else if (lastMessage?.role === "tool") {
        recordRequest("approval_after_tool");
        sendText(response, INITIAL_RUN_CANARY);
      } else {
        recordRequest("approval_initial");
        sendToolCall(response);
      }
    } catch (error) {
      sendJson(response, {
        error: error instanceof Error ? error.message : String(error),
      }, 500);
    }
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve();
    });
  });
  server.unref();
  const address = server.address();
  if (address === null || typeof address === "string") {
    throw new Error("desktop E2E provider did not bind a TCP port");
  }
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
  };
}

async function readRequestBody(request: IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  let totalBytes = 0;
  for await (const chunk of request) {
    const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    totalBytes += bytes.length;
    if (totalBytes > 2 * 1024 * 1024) {
      throw new Error("desktop E2E provider request exceeded 2 MiB");
    }
    chunks.push(bytes);
  }
  return Buffer.concat(chunks).toString("utf8");
}

function sendText(response: ServerResponse, content: string): void {
  sendSse(response, {
    delta: { content },
    finish_reason: "stop",
  });
}

function sendToolCall(response: ServerResponse): void {
  sendNamedToolCall(
    response,
    APPROVAL_CALL_ID,
    "bash",
    JSON.stringify({
      command: "printf 'desktop approval accepted\\n' > desktop-e2e-approved.txt",
    }),
  );
}

function sendNamedToolCall(
  response: ServerResponse,
  callId: string,
  name: string,
  argumentsJson: string,
): void {
  sendSse(response, {
    delta: {
      tool_calls: [{
        index: 0,
        id: callId,
        type: "function",
        function: {
          name,
          arguments: argumentsJson,
        },
      }],
    },
    finish_reason: "tool_calls",
  });
}

function sendSse(response: ServerResponse, choice: object): void {
  const body = `data: ${JSON.stringify({ choices: [choice] })}\n\ndata: [DONE]\n\n`;
  response.writeHead(200, {
    "cache-control": "no-cache",
    "content-length": Buffer.byteLength(body),
    "content-type": "text/event-stream",
  });
  response.end(body);
}

function sendJson(response: ServerResponse, payload: object, status = 200): void {
  const body = JSON.stringify(payload);
  response.writeHead(status, {
    "content-length": Buffer.byteLength(body),
    "content-type": "application/json",
  });
  response.end(body);
}

export const desktopProviderCanaries = {
  approvalCallId: APPROVAL_CALL_ID,
  initialRun: INITIAL_RUN_CANARY,
  agentRun: AGENT_RUN_CANARY,
  autoOrchestrationFinal: AUTO_ORCHESTRATION_FINAL_CANARY,
  autoOrchestrationPrompt: AUTO_ORCHESTRATION_PROMPT,
  autoReadStepIds: AUTO_READ_STEP_IDS,
  planDraftSummary: PLAN_REVIEW_DRAFT_SUMMARY,
  planRun: PLAN_RUN_CANARY,
  queuedPrompt: QUEUED_PROMPT,
  queuedRun: QUEUED_RUN_CANARY,
  skillRun: SKILL_RUN_CANARY,
  terminalLifecycleFinal: TERMINAL_LIFECYCLE_FINAL_CANARY,
  terminalLifecyclePrompt: TERMINAL_LIFECYCLE_PROMPT,
  terminalLifecycleReady: TERMINAL_LIFECYCLE_READY_CANARY,
  terminalSuccessorFinal: TERMINAL_SUCCESSOR_FINAL_CANARY,
  terminalSuccessorPrompt: TERMINAL_SUCCESSOR_PROMPT,
  title: TITLE_CANARY,
} as const;
