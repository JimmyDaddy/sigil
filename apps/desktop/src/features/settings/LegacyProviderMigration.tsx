import { useState } from "react";

import type { DesktopBridge } from "../../bridge";
import { useLocale } from "../../i18n";
import type {
  ProviderConnectionInventory,
  ProviderLegacyMigrationResult,
} from "../../types";
import { Button } from "../../ui/primitives";

type MigrationState =
  | "ready"
  | "migrating"
  | "reloading"
  | "retry_ready"
  | "reload_failed";
type MigrationFailureKind = "retryable" | "stale" | "attention";
export type ProviderMigrationRecoveryBlock =
  | "provider_migration_reconcile_required"
  | "provider_migration_rollback_incomplete"
  | "provider_migration_recovery_unavailable"
  | "provider_migration_blocked";

export function LegacyProviderMigration({
  bridge,
  workspaceId,
  inventory,
  mode,
  onMigrated,
  onInventoryReloaded,
  onOpenDiagnostics,
  recoveryBlock,
  onRecoveryBlocked,
  onRecoveryResolved,
  onContinue,
}: {
  readonly bridge: DesktopBridge;
  readonly workspaceId: string;
  readonly inventory: ProviderConnectionInventory;
  readonly mode: "onboarding" | "settings";
  readonly onMigrated: (result: ProviderLegacyMigrationResult) => void;
  readonly onInventoryReloaded: (inventory: ProviderConnectionInventory) => void;
  readonly onOpenDiagnostics: () => void;
  readonly recoveryBlock?: ProviderMigrationRecoveryBlock;
  readonly onRecoveryBlocked: (block: ProviderMigrationRecoveryBlock) => void;
  readonly onRecoveryResolved: () => void;
  readonly onContinue?: () => void;
}) {
  const { t } = useLocale();
  const [state, setState] = useState<MigrationState>("ready");
  const [failureKind, setFailureKind] = useState<MigrationFailureKind>("retryable");
  const inlineCredentialCount = inventory.legacyMigration?.inlineCredentialCount
    ?? inventory.connections.filter(
      (connection) => connection.credentialSource === "legacy_plaintext",
    ).length;
  const environmentReferenceCount = inventory.legacyMigration?.environmentReferenceCount
    ?? inventory.connections.filter(
      (connection) => connection.credentialSource === "environment",
    ).length;
  const connectionCount = inventory.legacyMigration?.connectionCount
    ?? inventory.connections.length;
  const defaultConnection = inventory.connections.find(
    (connection) => connection.id === inventory.defaultModel?.connectionId,
  );

  const reloadForRetry = async () => {
    setState("reloading");
    try {
      onInventoryReloaded(await bridge.providerConnections(workspaceId));
      setState("retry_ready");
    } catch {
      setState("reload_failed");
    }
  };

  const recheckRecovery = async () => {
    setState("reloading");
    try {
      const refreshed = await bridge.recheckLegacyProviderMigration(workspaceId);
      onInventoryReloaded(refreshed);
      const recoveryIsResolved = !refreshed.issues.some((issue) =>
        isMigrationRecoveryBlock(issue.code)
      );
      if (recoveryIsResolved) {
        onRecoveryResolved();
        setState("ready");
      } else {
        setState("ready");
      }
    } catch {
      setState("reload_failed");
    }
  };

  const migrate = async () => {
    setState("migrating");
    try {
      const revision = inventory.legacyMigration?.revision;
      if (revision === undefined) {
        await reloadForRetry();
        return;
      }
      const result = await bridge.migrateLegacyProviderConnections(workspaceId, revision);
      onMigrated(result);
    } catch (error) {
      const kind = migrationFailureKind(error);
      setFailureKind(kind);
      if (kind === "attention") {
        onRecoveryBlocked(
          migrationFailureCode(error) as ProviderMigrationRecoveryBlock,
        );
        setState("ready");
        return;
      }
      await reloadForRetry();
    }
  };

  const recoveryBlocked = recoveryBlock !== undefined;
  const reloading = state === "reloading";
  const showRetryFailure = state === "retry_ready";
  const reloadFailed = state === "reload_failed";

  return (
    <section
      className={`provider-legacy-migration provider-legacy-migration-${mode}`}
      aria-labelledby={`provider-legacy-migration-${mode}`}
      aria-busy={state === "migrating" || reloading}
    >
      <p className="eyebrow">{t("legacyMigrationEyebrow")}</p>
      {mode === "onboarding" ? (
        <h1 id={`provider-legacy-migration-${mode}`}>{t("legacyMigrationTitle")}</h1>
      ) : (
        <h3 id={`provider-legacy-migration-${mode}`}>{t("legacyMigrationTitle")}</h3>
      )}
      <p>{t("legacyMigrationDetail")}</p>
      <dl className="provider-legacy-migration-summary">
        <div>
          <dt>{t("legacyMigrationConnections")}</dt>
          <dd>{connectionCount}</dd>
        </div>
        <div>
          <dt>{t("legacyMigrationInlineKeys")}</dt>
          <dd>{t("legacyMigrationInlineKeysValue", { count: inlineCredentialCount })}</dd>
        </div>
        <div>
          <dt>{t("legacyMigrationEnvironmentRefs")}</dt>
          <dd>{t("legacyMigrationEnvironmentRefsValue", { count: environmentReferenceCount })}</dd>
        </div>
        <div>
          <dt>{t("legacyMigrationDefault")}</dt>
          <dd>
            {defaultConnection === undefined || inventory.defaultModel === undefined
              ? t("unavailable")
              : `${defaultConnection.label} / ${inventory.defaultModel.modelId}`}
          </dd>
        </div>
      </dl>
      <p className="provider-legacy-migration-note">{t("legacyMigrationPreserved")}</p>
      {recoveryBlocked ? (
        <p className="provider-setup-error" role="alert">
          {t("legacyMigrationAttention")}
        </p>
      ) : showRetryFailure ? (
        <p className="provider-setup-error" role="alert">
          {failureKind === "stale"
            ? t("legacyMigrationStale")
            : t("legacyMigrationFailed")}
        </p>
      ) : reloadFailed ? (
        <p className="provider-setup-error" role="alert">
          {t("legacyMigrationReloadFailed")}
        </p>
      ) : reloading ? (
        <p className="provider-setup-status" role="status">
          {t("legacyMigrationReloading")}
        </p>
      ) : null}
      <div className="provider-setup-actions">
        {onContinue === undefined || recoveryBlocked ? null : (
          <Button
            type="button"
            variant="quiet"
            disabled={state === "migrating" || reloading}
            onClick={onContinue}
          >
            {t("legacyMigrationContinue")}
          </Button>
        )}
        {showRetryFailure || reloadFailed || recoveryBlocked ? (
          <Button
            type="button"
            variant="quiet"
            disabled={reloading}
            onClick={onOpenDiagnostics}
          >
            {t("openSupport")}
          </Button>
        ) : null}
        <Button
          type="button"
          variant="primary"
          disabled={state === "migrating" || reloading}
          onClick={recoveryBlocked
            ? () => void recheckRecovery()
            : reloadFailed
              ? () => void reloadForRetry()
              : () => void migrate()}
        >
          {recoveryBlocked || reloadFailed
            ? t("recheckProviderConfig")
            : state === "migrating"
            ? t("legacyMigrating")
            : reloading
              ? t("legacyMigrationReloading")
              : showRetryFailure
              ? t("legacyMigrationRetry")
              : t("legacyMigrationConfirm")}
        </Button>
      </div>
    </section>
  );
}

function migrationFailureKind(error: unknown): MigrationFailureKind {
  const code = migrationFailureCode(error);
  if (code === "provider_migration_stale" || code === "provider_migration_not_required") {
    return "stale";
  }
  if (
    code === "provider_migration_reconcile_required"
    || code === "provider_migration_rollback_incomplete"
    || code === "provider_migration_recovery_unavailable"
    || code === "provider_migration_blocked"
  ) {
    return "attention";
  }
  return "retryable";
}

export function isMigrationRecoveryBlock(
  code: string | undefined,
): code is ProviderMigrationRecoveryBlock {
  return code === "provider_migration_reconcile_required"
    || code === "provider_migration_rollback_incomplete"
    || code === "provider_migration_recovery_unavailable"
    || code === "provider_migration_blocked";
}

function migrationFailureCode(error: unknown): string | undefined {
  return typeof error === "object"
    && error !== null
    && "code" in error
    && typeof error.code === "string"
    ? error.code
    : undefined;
}
