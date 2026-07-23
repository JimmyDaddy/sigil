const REOPEN_LAST_WORKSPACE_KEY = "sigil.desktop.reopen-last-workspace.v1";
const DEFAULT_MODELS_KEY = "sigil.desktop.default-models.v1";
const LAST_SESSIONS_KEY = "sigil.desktop.last-sessions.v1";

export interface LastSessionPreference {
  readonly sessionRef: string;
  readonly sessionId: string;
  readonly label?: string;
}

export function readReopenLastWorkspace(): boolean {
  try {
    return window.localStorage.getItem(REOPEN_LAST_WORKSPACE_KEY) !== "false";
  } catch {
    return true;
  }
}

export function writeReopenLastWorkspace(enabled: boolean): boolean {
  try {
    window.localStorage.setItem(REOPEN_LAST_WORKSPACE_KEY, String(enabled));
    return true;
  } catch {
    return false;
  }
}

export function readDefaultModel(workspaceId: string): string | undefined {
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(DEFAULT_MODELS_KEY) ?? "{}");
    if (!isStringRecord(value)) return undefined;
    const model = value[workspaceId];
    return typeof model === "string" && model.trim() !== "" ? model : undefined;
  } catch {
    return undefined;
  }
}

export function writeDefaultModel(workspaceId: string, modelName?: string): boolean {
  try {
    const current: unknown = JSON.parse(window.localStorage.getItem(DEFAULT_MODELS_KEY) ?? "{}");
    const models = isStringRecord(current) ? current : {};
    const next = { ...models };
    if (modelName === undefined) delete next[workspaceId];
    else next[workspaceId] = modelName;
    window.localStorage.setItem(DEFAULT_MODELS_KEY, JSON.stringify(next));
    return true;
  } catch {
    return false;
  }
}

export function readLastSession(workspaceId: string): LastSessionPreference | undefined {
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(LAST_SESSIONS_KEY) ?? "{}");
    if (!isUnknownRecord(value)) return undefined;
    return parseLastSession(value[workspaceId]);
  } catch {
    return undefined;
  }
}

export function writeLastSession(
  workspaceId: string,
  session?: LastSessionPreference,
): boolean {
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(LAST_SESSIONS_KEY) ?? "{}");
    const current = isUnknownRecord(value) ? value : {};
    const next = { ...current };
    if (session === undefined) delete next[workspaceId];
    else next[workspaceId] = session;
    window.localStorage.setItem(LAST_SESSIONS_KEY, JSON.stringify(next));
    return true;
  } catch {
    return false;
  }
}

function isStringRecord(value: unknown): value is Record<string, string> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isUnknownRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseLastSession(value: unknown): LastSessionPreference | undefined {
  if (!isUnknownRecord(value)) return undefined;
  const sessionRef = value.sessionRef;
  const sessionId = value.sessionId;
  const label = value.label;
  if (
    typeof sessionRef !== "string"
    || sessionRef.trim() === ""
    || typeof sessionId !== "string"
    || sessionId.trim() === ""
    || (label !== undefined && typeof label !== "string")
  ) {
    return undefined;
  }
  return {
    sessionRef,
    sessionId,
    ...(typeof label === "string" && label.trim() !== "" ? { label } : {}),
  };
}
