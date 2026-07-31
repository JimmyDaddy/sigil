use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result as AnyhowResult, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION, ConversationRunLifecycleRecordV1,
    INTENT_CANONICAL_DIGEST_PREFIX, IntentDigest, IntentDropRequestV1, IntentOperationAuthorityV1,
    IntentOperationExecutionV1, IntentOperationPreviewV1, IntentVersionRef, JsonlSessionStore,
    PermissionMode, PublicIntentStackStateV1, RootConfig, Session, WorkspaceTrust,
    conversation_run_lifecycle_record_from_stream, execute_intent_drop, preview_intent_drop,
    resolve_workspace_root, workspace_trust_from_entries,
};
use thiserror::Error;

use crate::{
    current_unix_time_ms,
    provider_connections::{resolve_default_model_route, validate_persisted_model_route},
};

/// Maximum lifetime of one host-owned confirmation authority.
pub const APPLICATION_INTENT_DROP_CONFIRMATION_TTL_MS: u64 = 5 * 60 * 1_000;

/// Adapter-neutral Intent Stack command.
///
/// The command intentionally contains no path, patch bytes, current file hash, permission policy
/// or approval authority. TUI, HTTP, Desktop and automation adapters all submit this same shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationIntentStackCommandV1 {
    Inspect,
    PreviewDrop { intent_ref: IntentVersionRef },
    ExecuteDrop { request: IntentDropRequestV1 },
}

/// Adapter-neutral result of one Intent Stack command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationIntentStackCommandOutputV1 {
    Projection {
        state: PublicIntentStackStateV1,
    },
    DropPreview {
        preview: IntentOperationPreviewV1,
    },
    DropExecution {
        execution: IntentOperationExecutionV1,
    },
}

/// Host surface that received the explicit confirmation.
///
/// This type has no serde implementation so a renderer or model payload cannot choose the
/// approval-authority namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationIntentConfirmationSource {
    Tui,
    Http,
    Automation,
}

impl ApplicationIntentConfirmationSource {
    const fn authority_prefix(self) -> &'static str {
        match self {
            Self::Tui => "tui-confirmed",
            Self::Http => "http-confirmed",
            Self::Automation => "automation-confirmed",
        }
    }

    const fn safe_reason(self) -> &'static str {
        match self {
            Self::Tui => "user confirmed exact TUI Intent drop preview",
            Self::Http => "user confirmed exact HTTP Intent drop preview",
            Self::Automation => "user confirmed exact automation Intent drop preview",
        }
    }
}

/// Stable error class shared by typed application adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationIntentStackErrorClass {
    InvalidRequest,
    Stale,
    PermissionRequired,
    Conflict,
    Unavailable,
}

/// Typed failure from the canonical Intent Stack application command.
///
/// The source remains host-only. Renderer and protocol adapters receive only the stable class,
/// so they never need to infer authority or stale state from error text.
#[derive(Debug, Error)]
pub enum ApplicationIntentStackError {
    #[error("invalid Intent Stack request: {source}")]
    InvalidRequest {
        #[source]
        source: anyhow::Error,
    },
    #[error("Intent Stack request is stale: {source}")]
    Stale {
        #[source]
        source: anyhow::Error,
    },
    #[error("Intent Stack permission is required: {source}")]
    PermissionRequired {
        #[source]
        source: anyhow::Error,
    },
    #[error("Intent Stack operation conflicts with current durable state: {source}")]
    Conflict {
        #[source]
        source: anyhow::Error,
    },
    #[error("Intent Stack is unavailable: {source}")]
    Unavailable {
        #[source]
        source: anyhow::Error,
    },
}

impl ApplicationIntentStackError {
    /// Returns the stable adapter-facing class.
    #[must_use]
    pub const fn class(&self) -> ApplicationIntentStackErrorClass {
        match self {
            Self::InvalidRequest { .. } => ApplicationIntentStackErrorClass::InvalidRequest,
            Self::Stale { .. } => ApplicationIntentStackErrorClass::Stale,
            Self::PermissionRequired { .. } => ApplicationIntentStackErrorClass::PermissionRequired,
            Self::Conflict { .. } => ApplicationIntentStackErrorClass::Conflict,
            Self::Unavailable { .. } => ApplicationIntentStackErrorClass::Unavailable,
        }
    }

    fn invalid_request(source: impl Into<anyhow::Error>) -> Self {
        Self::InvalidRequest {
            source: source.into(),
        }
    }

    fn stale(source: impl Into<anyhow::Error>) -> Self {
        Self::Stale {
            source: source.into(),
        }
    }

    fn permission_required(source: impl Into<anyhow::Error>) -> Self {
        Self::PermissionRequired {
            source: source.into(),
        }
    }

    fn conflict(source: impl Into<anyhow::Error>) -> Self {
        Self::Conflict {
            source: source.into(),
        }
    }

    fn unavailable(source: impl Into<anyhow::Error>) -> Self {
        Self::Unavailable {
            source: source.into(),
        }
    }
}

/// Executes the canonical Intent Stack command against an already loaded durable session.
///
/// Callers that own a live in-memory session remain responsible for excluding an active
/// foreground run. Write authority is always reconstructed here from current host configuration
/// and durable workspace trust.
pub fn execute_application_intent_stack_command(
    session: &Session,
    root_config: &RootConfig,
    workspace_root: &Path,
    command: &ApplicationIntentStackCommandV1,
    confirmation_source: ApplicationIntentConfirmationSource,
) -> Result<ApplicationIntentStackCommandOutputV1, ApplicationIntentStackError> {
    match command {
        ApplicationIntentStackCommandV1::Inspect => {
            let state = session
                .public_intent_stack_state_for_workspace(workspace_root)
                .map_err(ApplicationIntentStackError::unavailable)?;
            Ok(ApplicationIntentStackCommandOutputV1::Projection { state })
        }
        ApplicationIntentStackCommandV1::PreviewDrop { intent_ref } => {
            intent_ref
                .validate()
                .map_err(ApplicationIntentStackError::invalid_request)?;
            let preview = preview_intent_drop(session, workspace_root, intent_ref)
                .map_err(ApplicationIntentStackError::conflict)?;
            Ok(ApplicationIntentStackCommandOutputV1::DropPreview { preview })
        }
        ApplicationIntentStackCommandV1::ExecuteDrop { request } => {
            if root_config.permission.mode == PermissionMode::ReadOnly {
                return Err(ApplicationIntentStackError::permission_required(anyhow!(
                    "read-only permission mode denies Intent drop"
                )));
            }
            if workspace_trust_from_entries(session.entries(), workspace_root)
                .map_err(ApplicationIntentStackError::unavailable)?
                != WorkspaceTrust::Trusted
            {
                return Err(ApplicationIntentStackError::permission_required(anyhow!(
                    "workspace trust is required before Intent drop"
                )));
            }
            let state = session
                .public_intent_stack_state_for_workspace(workspace_root)
                .map_err(ApplicationIntentStackError::unavailable)?;
            match state {
                PublicIntentStackStateV1::Available { stack, .. }
                    if stack.stack_version != request.stack_version =>
                {
                    return Err(ApplicationIntentStackError::stale(anyhow!(
                        "stack version changed"
                    )));
                }
                PublicIntentStackStateV1::Available { .. } => {}
                PublicIntentStackStateV1::NotCreated { .. } => {
                    return Err(ApplicationIntentStackError::conflict(anyhow!(
                        "no Intent Stack exists in this session"
                    )));
                }
            }
            let authority = application_intent_drop_authority(
                root_config,
                request,
                confirmation_source,
                current_unix_time_ms(),
            )
            .map_err(ApplicationIntentStackError::unavailable)?;
            let execution = execute_intent_drop(
                session,
                workspace_root,
                request,
                &authority,
                confirmation_source.safe_reason(),
            )
            .map_err(ApplicationIntentStackError::conflict)?;
            Ok(ApplicationIntentStackCommandOutputV1::DropExecution { execution })
        }
    }
}

/// Loads a scope-bound durable application session and executes the canonical command.
///
/// This is the shared entry point for process adapters that do not own a live in-memory session.
/// It rejects symlink/non-file session paths and validates the expected durable scope before any
/// projection or mutation. State-sensitive preview/execute commands also fail closed when the
/// durable lifecycle still has an active foreground run; read-only inspection remains available
/// for recovery UI.
pub fn execute_durable_application_intent_stack_command(
    config_path: &Path,
    launch_cwd: &Path,
    session_log_path: &Path,
    expected_session_scope_id: &str,
    command: &ApplicationIntentStackCommandV1,
    confirmation_source: ApplicationIntentConfirmationSource,
) -> Result<ApplicationIntentStackCommandOutputV1, ApplicationIntentStackError> {
    validate_durable_intent_stack_session_path(session_log_path)
        .map_err(ApplicationIntentStackError::unavailable)?;
    if !matches!(command, ApplicationIntentStackCommandV1::Inspect) {
        ensure_durable_intent_stack_session_idle(session_log_path)
            .map_err(ApplicationIntentStackError::conflict)?;
    }
    let (root_config, workspace_root, session) = load_intent_stack_session(
        config_path,
        launch_cwd,
        session_log_path,
        expected_session_scope_id,
    )
    .map_err(ApplicationIntentStackError::unavailable)?;
    execute_application_intent_stack_command(
        &session,
        &root_config,
        &workspace_root,
        command,
        confirmation_source,
    )
}

fn ensure_durable_intent_stack_session_idle(session_log_path: &Path) -> AnyhowResult<()> {
    let records = JsonlSessionStore::read_event_records(session_log_path)
        .context("failed to read Intent Stack session lifecycle")?;
    let mut active_run_id: Option<String> = None;
    for record in &records {
        let Some(lifecycle) = conversation_run_lifecycle_record_from_stream(record)? else {
            continue;
        };
        match lifecycle {
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started) => {
                if started.schema_version() != CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION
                    || active_run_id.replace(started.run_id().to_owned()).is_some()
                {
                    bail!("Intent Stack session has ambiguous active run ownership");
                }
            }
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized) => {
                if finalized.schema_version() != CONVERSATION_RUN_LIFECYCLE_SCHEMA_VERSION
                    || active_run_id.as_deref() != Some(finalized.run_id())
                {
                    bail!("Intent Stack session terminal does not match active ownership");
                }
                active_run_id = None;
            }
        }
    }
    if active_run_id.is_some() {
        bail!("Intent Stack mutation is unavailable while a foreground run is active");
    }
    Ok(())
}

fn validate_durable_intent_stack_session_path(session_log_path: &Path) -> AnyhowResult<PathBuf> {
    let metadata = fs::symlink_metadata(session_log_path).with_context(|| {
        format!(
            "failed to inspect Intent Stack session {}",
            session_log_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Intent Stack session must be an existing regular non-symlink file");
    }
    session_log_path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize Intent Stack session {}",
            session_log_path.display()
        )
    })
}

fn load_intent_stack_session(
    config_path: &Path,
    launch_cwd: &Path,
    session_log_path: &Path,
    expected_session_scope_id: &str,
) -> AnyhowResult<(RootConfig, PathBuf, Session)> {
    if expected_session_scope_id.trim().is_empty() {
        bail!("Intent Stack session scope is empty");
    }
    let canonical_session_path = validate_durable_intent_stack_session_path(session_log_path)?;
    let root_config = RootConfig::load(config_path)
        .with_context(|| "failed to load Intent Stack application configuration")?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let (fallback_provider, fallback_route) = resolve_default_model_route(&root_config)
        .map_err(anyhow::Error::new)
        .context("failed to resolve Intent Stack session route")?;
    let store = JsonlSessionStore::new(canonical_session_path)?;
    let mut session = Session::load_from_store_with_route(
        fallback_provider,
        fallback_route.model_ref.model_id.clone(),
        None,
        store,
    )?;
    let persisted_route = session
        .resolved_model_route()
        .context("Intent Stack durable session route is unavailable")?
        .clone();
    let provider_name = validate_persisted_model_route(&root_config, &persisted_route)
        .map_err(anyhow::Error::new)
        .context("Intent Stack durable session route is unavailable")?;
    if session.provider_name() != provider_name
        || session.model_name() != persisted_route.model_ref.model_id
    {
        bail!("Intent Stack durable session route drifted");
    }
    if session.session_scope_id() != expected_session_scope_id {
        bail!("Intent Stack durable session scope changed");
    }
    crate::attach_session_url_capability_store(&mut session)?;
    Ok((root_config, workspace_root, session))
}

fn application_intent_drop_authority(
    root_config: &RootConfig,
    request: &IntentDropRequestV1,
    confirmation_source: ApplicationIntentConfirmationSource,
    now_ms: u64,
) -> AnyhowResult<IntentOperationAuthorityV1> {
    let permission_json = serde_json::to_vec(&root_config.permission)
        .context("failed to encode current permission policy")?;
    let mut hasher = Sha256::new();
    hasher.update(b"sigil.intent.permission_policy.v1\0");
    hasher.update(permission_json);
    let digest = IntentDigest::new(format!(
        "{INTENT_CANONICAL_DIGEST_PREFIX}{:x}",
        hasher.finalize()
    ))?;
    IntentOperationAuthorityV1::new(
        digest,
        format!(
            "{}:{}",
            confirmation_source.authority_prefix(),
            request.operation_id.as_str()
        ),
        Some(now_ms.saturating_add(APPLICATION_INTENT_DROP_CONFIRMATION_TTL_MS)),
    )
}

#[cfg(test)]
#[path = "tests/application_intent_stack_tests.rs"]
mod tests;
