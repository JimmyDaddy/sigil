import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { once } from "node:events";

const TITLE_CANARY = "验证桌面审批队列与恢复";
const INITIAL_RUN_CANARY = "DESKTOP_E2E_INITIAL_DONE";
const QUEUED_RUN_CANARY = "DESKTOP_E2E_QUEUED_DONE";
const QUEUED_PROMPT = "继续验证排队消息";
const APPROVAL_QUEUE_PROMPT = "审批等待期间仍可排队消息";
const APPROVAL_CALL_ID = "desktop-e2e-approval-call";
const SKILL_INSTRUCTION_MARKER = "DESKTOP_E2E_SKILL_INSTRUCTION";
const AGENT_INSTRUCTION_MARKER = "DESKTOP_E2E_AGENT_INSTRUCTION";
const PLAN_INSTRUCTION_MARKER = "Research and plan before execution; do not edit files directly.";
const SKILL_RUN_CANARY = "DESKTOP_E2E_SKILL_DONE";
const AGENT_RUN_CANARY = "DESKTOP_E2E_AGENT_DONE";
const PLAN_RUN_CANARY = "DESKTOP_E2E_PLAN_DONE";
const AUTO_ORCHESTRATION_PROMPT = "DESKTOP_E2E_AUTO_ORCHESTRATION";
const AUTO_ORCHESTRATION_FINAL_CANARY = "DESKTOP_E2E_AUTO_ORCHESTRATION_DONE";
const APPROVED_PLAN_EXECUTION_PROMPT =
  "Execute the following user-approved Plan with the configured approval and verification requirements.";
const TERMINAL_LIFECYCLE_PROMPT = "DESKTOP_E2E_TERMINAL_LIFECYCLE";
const TERMINAL_LIFECYCLE_READY_CANARY = "DESKTOP_E2E_TERMINAL_READY";
const TERMINAL_LIFECYCLE_FINAL_CANARY = "DESKTOP_E2E_TERMINAL_FOREGROUND_DONE";
const TERMINAL_SUCCESSOR_PROMPT = "DESKTOP_E2E_TERMINAL_SUCCESSOR";
const TERMINAL_SUCCESSOR_FINAL_CANARY = "DESKTOP_E2E_TERMINAL_SUCCESSOR_DONE";
const DURABLE_USER_INPUT_PROMPT = "DESKTOP_E2E_DURABLE_USER_INPUT";
const DURABLE_USER_INPUT_CARD_PROMPT = "Choose the verification target before continuing.";
const DURABLE_USER_INPUT_QUESTION = "Which current-format recovery flow must this change verify?";
const DURABLE_USER_INPUT_OPTION = "Interrupted current sessions";
const DURABLE_USER_INPUT_FINAL_CANARY = "DESKTOP_E2E_DURABLE_USER_INPUT_DONE";
const ARTIFACT_PROMPT = "DESKTOP_E2E_LARGE_TOOL_ARTIFACT";
const ARTIFACT_FINAL_CANARY = "DESKTOP_E2E_LARGE_TOOL_ARTIFACT_DONE";
const SHELL_ERROR_PROMPT = "DESKTOP_E2E_LARGE_SHELL_ERROR";
const SHELL_ERROR_OUTPUT_CANARY = "DESKTOP_E2E_SHELL_ERROR_OUTPUT";
const SHELL_ERROR_FINAL_CANARY = "DESKTOP_E2E_LARGE_SHELL_ERROR_DONE";
const HIGH_DELTA_PROMPT = "DESKTOP_E2E_HIGH_DELTA";
const HIGH_DELTA_FINAL_CANARY = "DESKTOP_E2E_HIGH_DELTA_FINAL";
const PLAN_REVISION_GUIDANCE = "DESKTOP_E2E_PLAN_REVISION: clarify the verification target before revising.";
const PLAN_REVISION_QUESTION_PROMPT = "Confirm the verification target for this plan revision.";
const PLAN_REVISION_SUMMARY = "DESKTOP_E2E_PLAN_REVISED_WITH_CURRENT_RECOVERY";
const DURABLE_USER_INPUT_ARGS = JSON.stringify({
  prompt: DURABLE_USER_INPUT_CARD_PROMPT,
  questions: [{
    id: "verification",
    header: "Verification",
    question: DURABLE_USER_INPUT_QUESTION,
    description: "Choose the target that constrains the implementation.",
    required: true,
    field: {
      kind: "single_select",
      options: [
        { id: "current", label: "Current release" },
        { id: "interrupted", label: DURABLE_USER_INPUT_OPTION },
      ],
      allow_other: false,
    },
  }],
});
const AUTO_START_TASK_ARGS = JSON.stringify({ title: "Desktop E2E direct task" });
const PLAN_REVIEW_REQUEST_ARGS = JSON.stringify({
  reason_codes: ["explicit_review_intent", "architectural_tradeoff"],
});
const PLAN_REVIEW_DRAFT_SUMMARY = "DESKTOP_E2E_PLAN_DRAFT";
const PLAN_REVIEW_RESULT_CONTENT = `${PLAN_REVIEW_DRAFT_SUMMARY}

1. Inspect the runtime architecture and record the relevant boundaries.
2. Verify the resulting plan against the captured workspace evidence.`;
interface ChatMessage {
  readonly role?: string;
  readonly content?: unknown;
}

interface ChatCompletionRequest {
  readonly messages?: ChatMessage[];
  readonly tools?: unknown[];
}

interface FixtureEvidence {
  readonly requestCounts: Readonly<Record<string, number>>;
}

export interface DesktopProviderFixture {
  readonly baseUrl: string;
}

// The real chat adapter may encode ordinary text using multimodal content parts.
function fixtureMessageText(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content.flatMap((part: unknown) => {
    if (part === null || typeof part !== "object") return [];
    const block = part as { type?: unknown; text?: unknown };
    return block.type === "text" && typeof block.text === "string" ? [block.text] : [];
  }).join("\n");
}

export async function startDesktopProviderFixture(): Promise<DesktopProviderFixture> {
  const requestCounts = new Map<string, number>();
  let holdNextDirect = false;
  let directLifecycleFixture = false;
  let releaseHighDelta: (() => void) | undefined;
  const recordRequest = (kind: string) => {
    requestCounts.set(kind, (requestCounts.get(kind) ?? 0) + 1);
  };
  const server = createServer(async (request, response) => {
    let rawBody = "";
    try {
      if (request.method === "GET") {
        if (request.url?.endsWith("/__evidence")) {
          sendJson(response, {
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
        holdNextDirect = false;
        directLifecycleFixture = false;
        sendJson(response, { reset: true });
        return;
      }
      if (request.method === "POST" && request.url?.endsWith("/__hold-next-direct")) {
        holdNextDirect = true;
        directLifecycleFixture = true;
        sendJson(response, { held: true });
        return;
      }
      if (request.method === "POST" && request.url?.endsWith("/__release-high-delta")) {
        if (releaseHighDelta === undefined) {
          sendJson(response, { error: "no paused high-delta stream" }, 409);
          return;
        }
        releaseHighDelta();
        sendJson(response, { released: true });
        return;
      }
      if (request.method !== "POST" || !request.url?.endsWith("/chat/completions")) {
        sendJson(response, { error: "unexpected desktop E2E provider request" }, 404);
        return;
      }

      rawBody = await readRequestBody(request);
      const payload = JSON.parse(rawBody) as ChatCompletionRequest;
      const messages = (payload.messages ?? []).map((message) => ({
        ...message,
        content: fixtureMessageText(message.content),
      }));
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
      } else if (requestText.includes(HIGH_DELTA_PROMPT)) {
        recordRequest("high_delta_stream");
        const gate = new Promise<void>((resolve) => { releaseHighDelta = resolve; });
        response.writeHead(200, { "cache-control": "no-cache", "content-type": "text/event-stream" });
        const chunk = `data: ${JSON.stringify({ choices: [{ delta: { content: "x" }, finish_reason: null }] })}\n\n`;
        for (let batch = 1; batch <= 100; batch += 1) {
          if (!response.write(chunk.repeat(1000))) await once(response, "drain");
          requestCounts.set("high_delta_frames", batch * 1000);
        }
        const deadline = setTimeout(() => releaseHighDelta?.(), 90_000);
        try {
          await gate;
          response.end(`data: ${JSON.stringify({ choices: [{ delta: { content: `\n\n${HIGH_DELTA_FINAL_CANARY}` }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`);
        } finally {
          clearTimeout(deadline);
          releaseHighDelta = undefined;
        }
      } else if (
        toolNames.has("bash")
        && requestText.includes(SHELL_ERROR_PROMPT)
        && lastMessage?.role === "tool"
        && typeof lastMessage.content === "string"
        && lastMessage.content.includes("ordinary conversation routing accepted")
      ) {
        recordRequest("large_shell_error_run");
        sendNamedToolCall(
          response,
          "desktop-e2e-large-shell-error",
          "bash",
          JSON.stringify({
            command: `python3 -c 'import sys; sys.stderr.write("${SHELL_ERROR_OUTPUT_CANARY}\\n" + "e" * 34000); sys.exit(7)'`,
          }),
        );
      } else if (lastMessage?.role === "tool" && requestText.includes(SHELL_ERROR_PROMPT)) {
        recordRequest("large_shell_error_final");
        sendText(response, SHELL_ERROR_FINAL_CANARY);
      } else if (
        toolNames.has("read_file")
        && requestText.includes(ARTIFACT_PROMPT)
        && lastMessage?.role === "tool"
        && typeof lastMessage.content === "string"
        && lastMessage.content.includes("ordinary conversation routing accepted")
      ) {
        recordRequest("large_tool_artifact_read");
        sendNamedToolCall(
          response,
          "desktop-e2e-large-artifact-read",
          "read_file",
          JSON.stringify({ path: "desktop-e2e-large-output.txt" }),
        );
      } else if (lastMessage?.role === "tool" && requestText.includes(ARTIFACT_PROMPT)) {
        recordRequest("large_tool_artifact_final");
        sendText(response, ARTIFACT_FINAL_CANARY);
      } else if (
        requestText.includes(DURABLE_USER_INPUT_PROMPT)
        && lastMessage?.role === "tool"
        && typeof lastMessage.content === "string"
        && lastMessage.content.includes("ordinary conversation routing accepted")
      ) {
        recordRequest("durable_user_input_request");
        sendNamedToolCall(
          response,
          "desktop-e2e-durable-user-input",
          "request_user_input",
          DURABLE_USER_INPUT_ARGS,
        );
      } else if (
        lastMessage?.role === "tool"
        && requestText.includes(DURABLE_USER_INPUT_PROMPT)
      ) {
        recordRequest("durable_user_input_continuation");
        sendText(response, DURABLE_USER_INPUT_FINAL_CANARY);
      } else if (
        toolNames.has("request_plan_review")
        && requestText.includes(AUTO_ORCHESTRATION_PROMPT)
      ) {
        recordRequest("plan_review_request");
        sendNamedToolCall(
          response,
          "desktop-auto-plan-review",
          "request_plan_review",
          PLAN_REVIEW_REQUEST_ARGS,
        );
      } else if (
        requestText.includes(PLAN_REVISION_GUIDANCE)
        && toolNames.has("request_user_input")
        && (requestCounts.get("plan_revision_question") ?? 0) === 0
      ) {
        recordRequest("plan_revision_question");
        const question = JSON.parse(DURABLE_USER_INPUT_ARGS) as Record<string, unknown>;
        sendNamedToolCall(
          response,
          "desktop-e2e-plan-revision-question",
          "request_user_input",
          JSON.stringify({ ...question, prompt: PLAN_REVISION_QUESTION_PROMPT }),
        );
      } else if (toolNames.has("submit_plan_review_result")) {
        const revised = requestText.includes(PLAN_REVISION_GUIDANCE);
        const summary = revised ? PLAN_REVISION_SUMMARY : PLAN_REVIEW_DRAFT_SUMMARY;
        recordRequest("plan_review_result");
        if (revised) recordRequest("plan_revision_completed");
        sendNamedToolCall(
          response,
          "desktop-auto-plan-review-result",
          "submit_plan_review_result",
          JSON.stringify({
            schema_version: 1,
            outcome: "draft",
            content: revised
              ? `${summary}\n\nVerify interrupted current-format session recovery before implementation.`
              : PLAN_REVIEW_RESULT_CONTENT,
          }),
        );
      } else if (
        toolNames.has("start_task")
        && requestText.includes(AUTO_ORCHESTRATION_PROMPT)
      ) {
        recordRequest("auto_conversation");
        sendNamedToolCall(
          response,
          "desktop-auto-handoff",
          "start_task",
          AUTO_START_TASK_ARGS,
        );
      } else if (requestText.includes(APPROVED_PLAN_EXECUTION_PROMPT)) {
        recordRequest("approved_plan_direct_execution");
        if (holdNextDirect) {
          holdNextDirect = false;
          // The real native pause cancels this provider connection; the next attempt is free.
          await once(response, "close");
          return;
        }
        if (directLifecycleFixture) await new Promise((resolve) => setTimeout(resolve, 500));
        sendText(response, AUTO_ORCHESTRATION_FINAL_CANARY);
      } else if (requestText.includes(SKILL_INSTRUCTION_MARKER)) {
        recordRequest("workspace_skill");
        sendText(response, SKILL_RUN_CANARY);
      } else if (requestText.includes(AGENT_INSTRUCTION_MARKER)) {
        recordRequest("workspace_agent");
        sendText(response, AGENT_RUN_CANARY);
      } else if (requestText.includes(PLAN_INSTRUCTION_MARKER)) {
        recordRequest("plan_agent");
        sendText(response, PLAN_RUN_CANARY);
      } else if (requestText.includes(QUEUED_PROMPT)) {
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
            command: `printf '${TERMINAL_LIFECYCLE_READY_CANARY}\\n'; sleep 60; printf 'DESKTOP_E2E_TERMINAL_EXIT\\n'`,
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
        requestText.includes(APPROVAL_QUEUE_PROMPT)
        && (requestCounts.get("approval_initial") ?? 0) > 0
      ) {
        recordRequest("approval_after_tool");
        sendText(response, INITIAL_RUN_CANARY);
      } else if (
        lastMessage?.role === "tool"
        && typeof lastMessage.content === "string"
        && lastMessage.content.includes("ordinary conversation routing accepted")
        && (requestCounts.get("approval_initial") ?? 0) === 0
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
  planRevisionGuidance: PLAN_REVISION_GUIDANCE,
  planRevisionQuestionPrompt: PLAN_REVISION_QUESTION_PROMPT,
  planRevisionSummary: PLAN_REVISION_SUMMARY,
  highDeltaPrompt: HIGH_DELTA_PROMPT,
  highDeltaFinal: HIGH_DELTA_FINAL_CANARY,
  artifactFinal: ARTIFACT_FINAL_CANARY,
  artifactPrompt: ARTIFACT_PROMPT,
  approvalCallId: APPROVAL_CALL_ID,
  initialRun: INITIAL_RUN_CANARY,
  agentRun: AGENT_RUN_CANARY,
  autoOrchestrationFinal: AUTO_ORCHESTRATION_FINAL_CANARY,
  autoOrchestrationPrompt: AUTO_ORCHESTRATION_PROMPT,
  durableUserInputCardPrompt: DURABLE_USER_INPUT_CARD_PROMPT,
  durableUserInputFinal: DURABLE_USER_INPUT_FINAL_CANARY,
  durableUserInputOption: DURABLE_USER_INPUT_OPTION,
  durableUserInputPrompt: DURABLE_USER_INPUT_PROMPT,
  durableUserInputQuestion: DURABLE_USER_INPUT_QUESTION,
  planDraftSummary: PLAN_REVIEW_DRAFT_SUMMARY,
  planRun: PLAN_RUN_CANARY,
  queuedPrompt: QUEUED_PROMPT,
  queuedRun: QUEUED_RUN_CANARY,
  shellErrorFinal: SHELL_ERROR_FINAL_CANARY,
  shellErrorOutput: SHELL_ERROR_OUTPUT_CANARY,
  shellErrorPrompt: SHELL_ERROR_PROMPT,
  skillRun: SKILL_RUN_CANARY,
  terminalLifecycleFinal: TERMINAL_LIFECYCLE_FINAL_CANARY,
  terminalLifecyclePrompt: TERMINAL_LIFECYCLE_PROMPT,
  terminalLifecycleReady: TERMINAL_LIFECYCLE_READY_CANARY,
  terminalSuccessorFinal: TERMINAL_SUCCESSOR_FINAL_CANARY,
  terminalSuccessorPrompt: TERMINAL_SUCCESSOR_PROMPT,
  title: TITLE_CANARY,
} as const;
