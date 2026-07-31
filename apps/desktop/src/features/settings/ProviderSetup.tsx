import { useMemo, useRef, useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type {
  ProviderConnectionInventory,
  ProviderSetupCatalog,
  ProviderSetupCatalogInput,
  ProviderSetupCredentialSource,
  ProviderSetupProtocol,
  ProviderSetupTemplate,
} from "../../types";
import { Button, Radio, Select, TextField } from "../../ui/primitives";
import {
  loadAndCacheProviderCatalog,
  readProviderCatalogCache,
} from "./providerCatalogCache";

type SetupStep = "provider" | "authentication" | "model";
type SetupState = "idle" | "loading" | "refreshing" | "saving" | "error";

const PROVIDERS: readonly ProviderSetupTemplate[] = [
  "deep_seek",
  "open_ai",
  "anthropic",
  "gemini",
  "open_ai_compatible",
];

export function ProviderSetup({
  bridge,
  workspaceId,
  inventory,
  mode,
  onSaved,
  onCancel,
}: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly inventory: ProviderConnectionInventory;
  readonly mode: "onboarding" | "settings" | "repair";
  readonly onSaved: (inventory: ProviderConnectionInventory) => void;
  readonly onCancel?: () => void;
}) {
  const { t } = useLocale();
  const [step, setStep] = useState<SetupStep>("provider");
  const [state, setState] = useState<SetupState>("idle");
  const [template, setTemplate] = useState<ProviderSetupTemplate>();
  const [credentialSource, setCredentialSource] =
    useState<ProviderSetupCredentialSource>("secure_store");
  const [protocol, setProtocol] = useState<ProviderSetupProtocol>("chat_completions");
  const [endpoint, setEndpoint] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [catalog, setCatalog] = useState<ProviderSetupCatalog>();
  const [modelId, setModelId] = useState("");
  const [manualModelId, setManualModelId] = useState("");
  const [error, setError] = useState<string>();
  const catalogRequestRevision = useRef(0);

  const isCustom = template === "open_ai_compatible";
  const effectiveModelId = modelId === "__manual__" ? manualModelId.trim() : modelId;
  const canLoadModels = template !== undefined
    && (credentialSource !== "secure_store" || apiKey.trim().length > 0)
    && (!isCustom || endpoint.trim().length > 0);
  const canSave = effectiveModelId.length > 0
    && state === "idle"
    && catalog !== undefined
    && providerCatalogAllowsSave(catalog);
  const progress = step === "provider" ? 1 : step === "authentication" ? 2 : 3;

  const catalogInput = useMemo<ProviderSetupCatalogInput | undefined>(() => {
    if (template === undefined) return undefined;
    return {
      template,
      protocol: isCustom ? protocol : undefined,
      endpoint: isCustom ? endpoint.trim() : undefined,
      credentialSource,
      apiKey: credentialSource === "secure_store" ? apiKey.trim() : undefined,
      replaceInvalidConfig: mode === "repair",
    };
  }, [apiKey, credentialSource, endpoint, isCustom, mode, protocol, template]);

  const chooseProvider = (choice: ProviderSetupTemplate) => {
    invalidateCatalogDraft();
    setTemplate(choice);
    setCredentialSource("secure_store");
    setApiKey("");
    setStep("authentication");
  };

  const loadModels = async () => {
    if (!canLoadModels || catalogInput === undefined) return;
    const requestRevision = ++catalogRequestRevision.current;
    setState("loading");
    setError(undefined);
    try {
      const cached = await readProviderCatalogCache(workspaceId, catalogInput);
      if (requestRevision !== catalogRequestRevision.current) return;
      if (cached !== undefined) {
        const view = cached.stale ? staleCatalogView(cached.catalog) : cached.catalog;
        if (!applyCatalog(view)) return;
        if (cached.stale) {
          setState("refreshing");
          void loadAndCacheProviderCatalog(bridge, workspaceId, catalogInput)
            .then((next) => {
              if (requestRevision !== catalogRequestRevision.current) return;
              if (providerCatalogAllowsSave(next)) {
                applyCatalog(next);
                setError(undefined);
                setState("idle");
              } else {
                setState("error");
                setError(providerCatalogFailureMessage(next.state, t));
              }
            })
            .catch(() => {
              if (requestRevision !== catalogRequestRevision.current) return;
              setState("error");
              setError(t("providerCatalogRefreshFailed"));
            });
        } else {
          setState("idle");
        }
        return;
      }
      const next = await loadAndCacheProviderCatalog(
        bridge,
        workspaceId,
        catalogInput,
      );
      if (requestRevision !== catalogRequestRevision.current) return;
      if (!applyCatalog(next)) return;
      setState("idle");
    } catch {
      if (requestRevision !== catalogRequestRevision.current) return;
      setState("error");
      setError(t("providerCatalogLoadFailed"));
    }
  };

  function invalidateCatalogDraft() {
    catalogRequestRevision.current += 1;
    setCatalog(undefined);
    setModelId("");
    setManualModelId("");
    setError(undefined);
    setState("idle");
  }

  const applyCatalog = (next: ProviderSetupCatalog): boolean => {
    if (!providerCatalogAllowsDisplay(next)) {
      setCatalog(undefined);
      setModelId("");
      setState("error");
      setError(providerCatalogFailureMessage(next.state, t));
      setStep("authentication");
      return false;
    }
    setCatalog(next);
    setModelId((current) => {
      if (current !== "" && next.models.some((model) => model.modelId === current)) {
        return current;
      }
      return next.suggestedModel
        ?? next.models.find((model) => model.availability !== "configured_unavailable")?.modelId
        ?? (next.manualEntryAllowed ? "__manual__" : "");
    });
    setStep("model");
    return true;
  };

  const save = async () => {
    if (!canSave || catalogInput === undefined) return;
    setState("saving");
    setError(undefined);
    try {
      const result = await bridge.saveProviderSetup(workspaceId, {
        ...catalogInput,
        modelId: effectiveModelId,
      });
      setApiKey("");
      setState("idle");
      onSaved(result.inventory);
    } catch {
      setState("error");
      setError(t("providerSetupSaveFailed"));
    }
  };

  return (
    <section
      className={`provider-setup provider-setup-${mode}`}
      aria-labelledby="provider-setup-title"
      aria-busy={
        state === "loading" || state === "refreshing" || state === "saving" || undefined
      }
    >
      <header className="provider-setup-header">
        <div>
          <p className="eyebrow">{t("providerSetupProgress", { current: progress, total: 3 })}</p>
          <h1 id="provider-setup-title">
            {mode === "onboarding"
              ? t("providerSetupTitle")
              : mode === "repair"
                ? t("replaceProviderConfig")
                : t("addProviderConnection")}
          </h1>
          <p>{mode === "repair" ? t("replaceProviderConfigDetail") : t("providerSetupDetail")}</p>
        </div>
        {mode !== "onboarding" && onCancel !== undefined ? (
          <Button type="button" variant="quiet" onClick={onCancel}>{t("cancel")}</Button>
        ) : null}
      </header>

      {step === "provider" ? (
        <div className="provider-choice-grid" role="list" aria-label={t("chooseProvider")}>
          {PROVIDERS.map((provider) => (
            <Button
              key={provider}
              type="button"
              variant="secondary"
              className="provider-choice"
              onClick={() => chooseProvider(provider)}
            >
              <span>
                <strong>{providerName(provider, t)}</strong>
                <small>{providerDescription(provider, t)}</small>
              </span>
            </Button>
          ))}
        </div>
      ) : null}

      {step === "authentication" && template !== undefined ? (
        <div className="provider-setup-form">
          <div className="provider-setup-selection">
            <strong>{providerName(template, t)}</strong>
            <Button type="button" variant="quiet" onClick={() => setStep("provider")}>
              {t("change")}
            </Button>
          </div>
          {isCustom ? (
            <>
              <TextField
                label={t("providerEndpoint")}
                description={t("providerEndpointDetail")}
                value={endpoint}
                placeholder="http://127.0.0.1:11434/v1"
                onChange={(event) => {
                  invalidateCatalogDraft();
                  setEndpoint(event.currentTarget.value);
                }}
              />
              <Select
                label={t("providerProtocol")}
                value={protocol}
                onChange={(event) => {
                  invalidateCatalogDraft();
                  setProtocol(event.currentTarget.value as ProviderSetupProtocol);
                }}
              >
                <option value="chat_completions">Chat Completions</option>
                <option value="responses">Responses</option>
              </Select>
            </>
          ) : null}
          <Select
            label={t("credentialSource")}
            description={t("credentialSourceDetail")}
            value={credentialSource}
            onChange={(event) => {
              invalidateCatalogDraft();
              setCredentialSource(
                event.currentTarget.value as ProviderSetupCredentialSource,
              );
            }}
          >
            <option value="secure_store">{t("secureStore")}</option>
            <option value="environment">{t("environmentVariable")}</option>
            {isCustom ? <option value="none">{t("noAuthentication")}</option> : null}
          </Select>
          {credentialSource === "secure_store" ? (
            <TextField
              type="password"
              autoComplete="off"
              spellCheck={false}
              label={t("apiKey")}
              description={t("apiKeySecureStoreDetail")}
              value={apiKey}
              onChange={(event) => {
                invalidateCatalogDraft();
                setApiKey(event.currentTarget.value);
              }}
            />
          ) : credentialSource === "environment" ? (
            <p className="provider-setup-note">
              {t("providerEnvironmentDetail", {
                variable: providerEnvironment(template, protocol),
              })}
            </p>
          ) : (
            <p className="provider-setup-note">{t("noAuthenticationDetail")}</p>
          )}
          <div className="provider-setup-actions">
            <Button type="button" onClick={() => setStep("provider")}>{t("back")}</Button>
            <Button
              type="button"
              variant="primary"
              disabled={!canLoadModels || state === "loading"}
              onClick={() => void loadModels()}
            >
              {state === "loading" ? t("loadingModels") : t("continueToModels")}
            </Button>
          </div>
        </div>
      ) : null}

      {step === "model" && catalog !== undefined && template !== undefined ? (
        <div className="provider-setup-form">
          <div className="provider-catalog-summary">
            <div>
              <strong>{catalog.providerLabel}</strong>
              <span>
                {t("providerCatalogState", {
                  state: providerCatalogStateLabel(catalog.state, t),
                })}
              </span>
            </div>
            <Button
              type="button"
              variant="quiet"
              onClick={() => {
                invalidateCatalogDraft();
                setStep("authentication");
              }}
            >
              {t("changeConnection")}
            </Button>
          </div>
          <fieldset className="provider-model-list">
            <legend>{t("chooseModel")}</legend>
            {catalog.models.map((model) => (
              <Radio
                key={model.modelId}
                name="provider-model"
                label={model.displayName}
                description={[
                  model.modelId,
                  model.recommended ? t("recommended") : undefined,
                  model.availability === "unverified" ? t("unverified") : undefined,
                ].filter(Boolean).join(" · ")}
                value={model.modelId}
                checked={modelId === model.modelId}
                disabled={model.availability === "configured_unavailable"}
                onChange={() => setModelId(model.modelId)}
              />
            ))}
            {catalog.manualEntryAllowed ? (
              <Radio
                name="provider-model"
                label={t("enterModelManually")}
                description={t("enterModelManuallyDetail")}
                value="__manual__"
                checked={modelId === "__manual__"}
                onChange={() => setModelId("__manual__")}
              />
            ) : null}
          </fieldset>
          {modelId === "__manual__" ? (
            <TextField
              label={t("modelId")}
              value={manualModelId}
              onChange={(event) => setManualModelId(event.currentTarget.value)}
            />
          ) : null}
          <div className="provider-setup-actions">
            <Button
              type="button"
              onClick={() => {
                invalidateCatalogDraft();
                setStep("authentication");
              }}
            >
              {t("back")}
            </Button>
            {catalog.state === "cache_stale" ? (
              <Button
                type="button"
                variant="secondary"
                disabled={state === "refreshing"}
                onClick={() => void loadModels()}
              >
                {state === "refreshing" ? t("refreshingModels") : t("retryModelCatalog")}
              </Button>
            ) : null}
            <Button
              type="button"
              variant="primary"
              disabled={!canSave}
              onClick={() => void save()}
            >
              {state === "saving"
                ? t("savingProvider")
                : mode === "repair"
                  ? t("replaceProviderConfigAndContinue")
                  : t("saveAndContinue")}
            </Button>
          </div>
        </div>
      ) : null}

      {error === undefined ? null : (
        <p className="provider-setup-error" role="alert">{error}</p>
      )}
      {inventory.issues.length === 0 ? null : (
        <p className="provider-setup-warning">{t("providerConfigNeedsRepair")}</p>
      )}
    </section>
  );
}

function providerName(
  provider: ProviderSetupTemplate,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (provider) {
    case "deep_seek": return "DeepSeek";
    case "open_ai": return "OpenAI";
    case "anthropic": return "Anthropic";
    case "gemini": return "Google Gemini";
    case "open_ai_compatible": return t("openAiCompatible");
  }
}

function providerDescription(
  provider: ProviderSetupTemplate,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (provider) {
    case "deep_seek": return t("deepSeekProviderDetail");
    case "open_ai": return t("openAiProviderDetail");
    case "anthropic": return t("anthropicProviderDetail");
    case "gemini": return t("geminiProviderDetail");
    case "open_ai_compatible": return t("customProviderDetail");
  }
}

function providerEnvironment(
  provider: ProviderSetupTemplate,
  protocol: ProviderSetupProtocol,
): string {
  switch (provider) {
    case "deep_seek": return "SIGIL_API_KEY";
    case "open_ai": return "SIGIL_OPENAI_RESPONSES_API_KEY";
    case "anthropic": return "SIGIL_ANTHROPIC_API_KEY";
    case "gemini": return "SIGIL_GEMINI_API_KEY";
    case "open_ai_compatible":
      return protocol === "responses"
        ? "SIGIL_OPENAI_RESPONSES_API_KEY"
        : "SIGIL_OPENAI_COMPATIBLE_API_KEY";
  }
}

function providerCatalogAllowsSave(catalog: ProviderSetupCatalog): boolean {
  return catalog.state === "remote"
    || catalog.state === "cache_fresh"
    || catalog.state === "remote_empty"
    || catalog.state === "catalog_unsupported";
}

function providerCatalogAllowsDisplay(catalog: ProviderSetupCatalog): boolean {
  return providerCatalogAllowsSave(catalog) || catalog.state === "cache_stale";
}

function staleCatalogView(catalog: ProviderSetupCatalog): ProviderSetupCatalog {
  return {
    ...catalog,
    state: "cache_stale",
    models: catalog.models.map((model) => ({
      ...model,
      availability: model.availability === "configured_unavailable"
        ? "configured_unavailable"
        : "unverified",
    })),
    manualEntryAllowed: false,
  };
}

function providerCatalogStateLabel(
  state: string,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (state) {
    case "remote": return t("providerCatalogVerified");
    case "cache_fresh": return t("providerCatalogCached");
    case "cache_stale": return t("providerCatalogStale");
    case "remote_empty": return t("providerCatalogEmpty");
    case "catalog_unsupported": return t("providerCatalogManual");
    default: return state;
  }
}

function providerCatalogFailureMessage(
  state: string,
  t: ReturnType<typeof useLocale>["t"],
): string {
  switch (state) {
    case "auth_rejected": return t("providerCatalogAuthRejected");
    case "credential_unavailable": return t("providerCatalogCredentialUnavailable");
    case "offline": return t("providerCatalogOffline");
    case "tls_rejected": return t("providerCatalogTlsRejected");
    case "protocol_mismatch": return t("providerCatalogProtocolMismatch");
    case "catalog_malformed": return t("providerCatalogMalformed");
    case "rate_limited": return t("providerCatalogRateLimited");
    default: return t("providerCatalogLoadFailed");
  }
}
