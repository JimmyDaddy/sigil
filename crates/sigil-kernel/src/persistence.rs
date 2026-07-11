use std::{collections::BTreeSet, fmt, sync::Arc};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{MessageRole, ModelMessage, NetworkEffect, SecretString, ToolCall, ToolRestartPolicy};

/// Hard limit for raw arguments retained while reconstructing one streamed tool call.
pub const MAX_STREAMED_TOOL_ARGS_BYTES: usize = 256 * 1024;
/// Hard limit across all raw tool arguments retained for one provider turn.
pub const MAX_PROVIDER_TURN_TOOL_ARGS_BYTES: usize = 1024 * 1024;
/// Hard limit for completed tool calls accepted in one provider turn.
pub const MAX_PROVIDER_TURN_TOOL_CALLS: usize = 128;
/// Hard limit for a provider tool-call id.
pub const MAX_TOOL_CALL_ID_BYTES: usize = 512;
/// Hard limit for a provider tool name.
pub const MAX_TOOL_CALL_NAME_BYTES: usize = 512;
/// Default durable validity window for one user-observed URL capability descriptor.
pub const DEFAULT_WEB_URL_CAPABILITY_TTL_MS: u64 = 3_600_000;
const MAX_WEB_URL_SAFE_DISPLAY_BYTES: usize = 2 * 1024;
const MAX_DURABLE_MESSAGE_ID_BYTES: usize = 512;

/// Provenance of one observed URL before it becomes a session-local capability.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebUrlProvenanceKind {
    UserMessage,
    WebSearchResult,
    PriorWebFetch,
    RedirectTarget,
}

/// Typed pre-persistence failure for unsafe or oversized tool-call arguments.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SafePersistenceError {
    #[error("tool_args_too_large: observed at least {observed_bytes} bytes, limit {limit_bytes}")]
    ToolArgsTooLarge {
        observed_bytes: usize,
        limit_bytes: usize,
    },
    #[error(
        "tool_call_identity_too_large: {field} observed {observed_bytes} bytes, limit {limit_bytes}"
    )]
    ToolCallIdentityTooLarge {
        field: &'static str,
        observed_bytes: usize,
        limit_bytes: usize,
    },
    #[error("tool_call_identity_unsafe: {field} must use a non-empty ASCII tool identity")]
    ToolCallIdentityUnsafe { field: &'static str },
    #[error("transient message overlay invariant failed: {reason}")]
    OverlayInvariant { reason: String },
    #[error("tool_call_stream_invalid: {reason}")]
    ToolCallStreamInvalid { reason: String },
}

/// One URL capability staged before the safe durable user entry is appended.
#[derive(Clone)]
pub struct UserUrlCapabilityRegistration {
    pub source_id: String,
    pub durable_entry_id: String,
    pub raw_canonical_url: SecretString,
    pub safe_display_url: String,
    pub restart_policy: ToolRestartPolicy,
    pub replayable_canonical_url: Option<String>,
    pub originating_call_id: Option<String>,
    pub provenance: WebUrlProvenanceKind,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Durable, secret-free descriptor proving that a live URL capability existed for a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WebUrlCapabilityDescriptor {
    pub session_scope_id: String,
    pub source_id: String,
    pub durable_entry_id: String,
    pub safe_display_url: String,
    pub restart_policy: ToolRestartPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayable_canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originating_call_id: Option<String>,
    pub provenance: WebUrlProvenanceKind,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl WebUrlCapabilityDescriptor {
    /// Validates restart semantics without deriving any value from the safe display URL.
    pub fn validate(&self) -> Result<()> {
        if self.session_scope_id.trim().is_empty()
            || self.source_id.trim().is_empty()
            || self.durable_entry_id.trim().is_empty()
            || self.safe_display_url.trim().is_empty()
        {
            bail!("URL capability descriptor identities and safe display must not be empty");
        }
        if self.session_scope_id.len() > 256
            || !is_session_local_source_id(&self.source_id)
            || self.durable_entry_id.len() > MAX_DURABLE_MESSAGE_ID_BYTES
            || self.safe_display_url.len() > MAX_WEB_URL_SAFE_DISPLAY_BYTES
        {
            bail!("URL capability descriptor identity or safe display is invalid or oversized");
        }
        if let Some(call_id) = self.originating_call_id.as_deref() {
            validate_tool_call_id(call_id)?;
        }
        if self.issued_at_ms == 0 || self.expires_at_ms <= self.issued_at_ms {
            bail!("URL capability descriptor expiry must be after its issue time");
        }
        match (
            self.restart_policy,
            self.replayable_canonical_url.as_deref(),
        ) {
            (ToolRestartPolicy::InterruptOnRestart, None) => {
                validate_interrupt_safe_display_url(&self.safe_display_url)
            }
            (ToolRestartPolicy::Replayable, Some(value)) => {
                let parsed = Url::parse(value)
                    .context("replayable URL capability value is not a valid URL")?;
                validate_observed_url(&parsed)?;
                if parsed.query().is_some()
                    || parsed.fragment().is_some()
                    || parsed.as_str() != value
                {
                    bail!("replayable URL capability must be canonical and queryless");
                }
                if self.safe_display_url != value {
                    bail!("replayable URL capability safe display must equal its canonical URL");
                }
                Ok(())
            }
            _ => bail!("URL capability restart policy contradicts replayable URL material"),
        }
    }
}

fn validate_interrupt_safe_display_url(value: &str) -> Result<()> {
    let parsed = Url::parse(value).context("interrupt URL safe display is not a valid URL")?;
    validate_observed_url(&parsed)?;
    if parsed.as_str() != value || parsed.fragment().is_some() {
        bail!("interrupt URL safe display must use canonical serialization without a fragment");
    }
    let redacted_query = parsed.query() == Some("[redacted]");
    let redacted_path = parsed.path() == "/[redacted]";
    if parsed.query().is_some() && !redacted_query {
        bail!("interrupt URL safe display contains raw query material");
    }
    if parsed.path().contains("[redacted]") && !redacted_path {
        bail!("interrupt URL safe display contains an invalid redacted path");
    }
    if !redacted_query && !redacted_path {
        bail!("interrupt URL safe display must carry a redacted query or path marker");
    }
    Ok(())
}

impl UserUrlCapabilityRegistration {
    #[must_use]
    pub fn durable_descriptor(
        &self,
        session_scope_id: impl Into<String>,
    ) -> WebUrlCapabilityDescriptor {
        WebUrlCapabilityDescriptor {
            session_scope_id: session_scope_id.into(),
            source_id: self.source_id.clone(),
            durable_entry_id: self.durable_entry_id.clone(),
            safe_display_url: self.safe_display_url.clone(),
            restart_policy: self.restart_policy,
            replayable_canonical_url: self.replayable_canonical_url.clone(),
            originating_call_id: self.originating_call_id.clone(),
            provenance: self.provenance,
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
        }
    }
}

impl fmt::Debug for UserUrlCapabilityRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserUrlCapabilityRegistration")
            .field("source_id", &self.source_id)
            .field("durable_entry_id", &self.durable_entry_id)
            .field("raw_canonical_url", &"[redacted]")
            .field("safe_display_url", &self.safe_display_url)
            .field("restart_policy", &self.restart_policy)
            .field("replayable_canonical_url", &self.replayable_canonical_url)
            .field("originating_call_id", &self.originating_call_id)
            .field("provenance", &self.provenance)
            .field("issued_at_ms", &self.issued_at_ms)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

/// Runtime-owned live capability sink used by the kernel's mandatory safe projection boundary.
///
/// Staging happens before durable append. The agent commits only after append succeeds and rolls
/// the stage back on failure. Implementations must make all three operations idempotent by durable
/// message id and source id.
pub trait UserUrlCapabilityRegistrar: Send + Sync {
    fn stage(&self, registration: UserUrlCapabilityRegistration) -> Result<()>;
    fn commit_message(&self, durable_entry_id: &str) -> Result<()>;
    fn rollback_message(&self, durable_entry_id: &str) -> Result<()>;
}

/// Non-serializable exact replacement for one safe durable provider-visible message.
#[derive(Clone)]
pub struct TransientMessageOverlay {
    durable_message_id: String,
    exact_message: ModelMessage,
}

impl TransientMessageOverlay {
    pub fn new(durable_message_id: impl Into<String>, exact_message: ModelMessage) -> Result<Self> {
        let durable_message_id = durable_message_id.into();
        if durable_message_id.trim().is_empty() || exact_message.id != durable_message_id {
            bail!(SafePersistenceError::OverlayInvariant {
                reason: "exact message id must equal the durable message id".to_owned(),
            });
        }
        Ok(Self {
            durable_message_id,
            exact_message,
        })
    }

    #[must_use]
    pub fn durable_message_id(&self) -> &str {
        &self.durable_message_id
    }

    fn exact_message(&self) -> &ModelMessage {
        &self.exact_message
    }
}

impl fmt::Debug for TransientMessageOverlay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransientMessageOverlay")
            .field("durable_message_id", &self.durable_message_id)
            .field("exact_message", &"[redacted transient message]")
            .finish()
    }
}

/// Mandatory safe durable user message plus its current-run exact replacement.
#[derive(Clone)]
pub struct UserMessagePersistenceProjection {
    pub durable_message: ModelMessage,
    pub overlay: TransientMessageOverlay,
    pub capability_registrations: Vec<UserUrlCapabilityRegistration>,
}

impl fmt::Debug for UserMessagePersistenceProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserMessagePersistenceProjection")
            .field("durable_message", &self.durable_message)
            .field("overlay", &self.overlay)
            .field(
                "capability_registration_count",
                &self.capability_registrations.len(),
            )
            .finish()
    }
}

/// Exact transient tool call and the only representation allowed in durable/event surfaces.
#[derive(Clone)]
pub struct ToolCallPersistenceProjection {
    pub durable_call: ToolCall,
    exact_call: ToolCall,
}

impl ToolCallPersistenceProjection {
    #[must_use]
    pub(crate) fn into_exact_call(self) -> ToolCall {
        self.exact_call
    }
}

impl fmt::Debug for ToolCallPersistenceProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCallPersistenceProjection")
            .field("durable_call", &self.durable_call)
            .field("exact_call", &"[redacted transient tool call]")
            .finish()
    }
}

/// Provider-neutral safe projection for a hosted intent before any session/event write.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct HostedIntentPersistenceProjection {
    pub authorization_id: String,
    pub provider_name: String,
    pub model_name: String,
    pub capability_fingerprint: String,
    pub network_effect: NetworkEffect,
}

/// Canonical URL classification shared by kernel projection and runtime capability validation.
#[derive(Clone, PartialEq, Eq)]
pub struct CanonicalWebUrlPersistenceProjection {
    pub raw_canonical_url: SecretString,
    pub safe_display_url: String,
    pub restart_policy: ToolRestartPolicy,
    pub replayable_canonical_url: Option<String>,
}

impl fmt::Debug for CanonicalWebUrlPersistenceProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalWebUrlPersistenceProjection")
            .field("raw_canonical_url", &"[redacted]")
            .field("safe_display_url", &self.safe_display_url)
            .field("restart_policy", &self.restart_policy)
            .field("replayable_canonical_url", &self.replayable_canonical_url)
            .finish()
    }
}

/// Parses and classifies an observed URL using the single persistence/restart policy source.
pub fn canonical_web_url_persistence_projection(
    raw_url: &str,
) -> Result<CanonicalWebUrlPersistenceProjection> {
    let parsed = Url::parse(raw_url).context("failed to parse observed URL")?;
    validate_observed_url(&parsed)?;
    let sensitive_path = url_has_sensitive_path(&parsed);
    let safe_display_url = safe_display_url(&parsed, sensitive_path);
    let replayable = parsed.query().is_none() && parsed.fragment().is_none() && !sensitive_path;
    Ok(CanonicalWebUrlPersistenceProjection {
        raw_canonical_url: SecretString::new(parsed.to_string()),
        safe_display_url,
        restart_policy: if replayable {
            ToolRestartPolicy::Replayable
        } else {
            ToolRestartPolicy::InterruptOnRestart
        },
        replayable_canonical_url: replayable.then(|| parsed.to_string()),
    })
}

/// Projects a raw user message at the kernel run boundary and stages any live URL capabilities.
///
/// The returned durable message and exact overlay share one identity. Failure to project or stage
/// any capability aborts the whole operation and rolls back all stages for that durable id.
pub fn project_user_message_for_persistence(
    durable_message_id: impl Into<String>,
    raw_message: impl Into<String>,
    registrar: Option<&Arc<dyn UserUrlCapabilityRegistrar>>,
) -> Result<UserMessagePersistenceProjection> {
    let durable_message_id = durable_message_id.into();
    if durable_message_id.trim().is_empty() {
        bail!("durable user message id must not be empty");
    }
    let raw_message = raw_message.into();
    project_user_message_for_persistence_with_nonce(
        durable_message_id,
        raw_message,
        None,
        registrar,
    )
}

/// Projects one user message while deriving random-looking source ids from a live-only nonce.
/// Cloned/retried run input reuses the nonce, while the nonce is never persisted or exposed.
pub fn project_user_message_for_persistence_with_nonce(
    durable_message_id: impl Into<String>,
    raw_message: impl Into<String>,
    source_id_nonce: Option<&str>,
    registrar: Option<&Arc<dyn UserUrlCapabilityRegistrar>>,
) -> Result<UserMessagePersistenceProjection> {
    project_user_message_for_persistence_with_nonce_and_issued_at(
        durable_message_id,
        raw_message,
        source_id_nonce,
        unix_time_ms(),
        registrar,
    )
}

/// Variant with an explicit issue time so cloned/retried run input preserves one descriptor.
pub fn project_user_message_for_persistence_with_nonce_and_issued_at(
    durable_message_id: impl Into<String>,
    raw_message: impl Into<String>,
    source_id_nonce: Option<&str>,
    issued_at_ms: u64,
    registrar: Option<&Arc<dyn UserUrlCapabilityRegistrar>>,
) -> Result<UserMessagePersistenceProjection> {
    let durable_message_id = durable_message_id.into();
    if durable_message_id.trim().is_empty() {
        bail!("durable user message id must not be empty");
    }
    let raw_message = raw_message.into();
    let (safe_content, capability_registrations) = project_text_urls(
        &durable_message_id,
        &raw_message,
        source_id_nonce,
        issued_at_ms,
        true,
    )?;
    let safe_content = redact_secret_carriers(&safe_content);
    if let Some(registrar) = registrar {
        for registration in &capability_registrations {
            if let Err(error) = registrar.stage(registration.clone()) {
                let rollback_error = registrar.rollback_message(&durable_message_id).err();
                return Err(error.context(match rollback_error {
                    Some(rollback_error) => format!(
                        "failed to stage user URL capability; rollback also failed: {rollback_error:#}"
                    ),
                    None => "failed to stage user URL capability".to_owned(),
                }));
            }
        }
    }
    let durable_message = ModelMessage {
        id: durable_message_id.clone(),
        role: MessageRole::User,
        content: Some(safe_content),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: None,
    };
    let exact_message = ModelMessage {
        id: durable_message_id.clone(),
        role: MessageRole::User,
        content: Some(raw_message),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: None,
    };
    let overlay = TransientMessageOverlay::new(durable_message_id, exact_message)?;
    Ok(UserMessagePersistenceProjection {
        durable_message,
        overlay,
        capability_registrations,
    })
}

/// Produces a safe durable/event representation of one exact tool call.
pub fn project_tool_call_for_persistence(
    exact_call: ToolCall,
) -> std::result::Result<ToolCallPersistenceProjection, SafePersistenceError> {
    validate_tool_call_identity(&exact_call)?;
    if exact_call.args_json.len() > MAX_STREAMED_TOOL_ARGS_BYTES {
        return Err(SafePersistenceError::ToolArgsTooLarge {
            observed_bytes: exact_call.args_json.len(),
            limit_bytes: MAX_STREAMED_TOOL_ARGS_BYTES,
        });
    }
    let durable_args = match serde_json::from_str::<Value>(&exact_call.args_json) {
        Ok(mut value) => {
            sanitize_json_value(&mut value);
            serde_json::to_string(&value).unwrap_or_else(|_| {
                json!({
                    "projection": "unavailable",
                    "raw_bytes": exact_call.args_json.len(),
                })
                .to_string()
            })
        }
        Err(_) => json!({
            "projection": "malformed_arguments",
            "raw_bytes": exact_call.args_json.len(),
        })
        .to_string(),
    };
    let durable_call = ToolCall {
        id: exact_call.id.clone(),
        name: exact_call.name.clone(),
        args_json: durable_args,
    };
    Ok(ToolCallPersistenceProjection {
        durable_call,
        exact_call,
    })
}

/// Projects any provider-visible message to a safe durable/snapshot representation while keeping
/// its exact form in a non-serializable current-run overlay.
pub fn project_message_for_persistence(
    exact_message: ModelMessage,
) -> std::result::Result<(ModelMessage, TransientMessageOverlay), SafePersistenceError> {
    if exact_message.id.trim().is_empty() {
        return Err(SafePersistenceError::OverlayInvariant {
            reason: "provider-visible message id must not be empty".to_owned(),
        });
    }
    let mut durable_message = exact_message.clone();
    durable_message.content = exact_message.content.as_deref().map(safe_persistence_text);
    durable_message.tool_calls = exact_message
        .tool_calls
        .iter()
        .cloned()
        .map(project_tool_call_for_persistence)
        .map(|projection| projection.map(|projection| projection.durable_call))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let overlay =
        TransientMessageOverlay::new(exact_message.id.clone(), exact_message).map_err(|error| {
            SafePersistenceError::OverlayInvariant {
                reason: format!("failed to construct exact message overlay: {error:#}"),
            }
        })?;
    Ok((durable_message, overlay))
}

/// Applies exact current-run overlays only after safe request materialization and snapshot writes.
///
/// Every overlay must match exactly one safe message by id. Duplicate overlays, zero matches, or
/// more than one match fail closed.
pub fn apply_exact_message_overlays(
    safe_messages: &[ModelMessage],
    overlays: &[TransientMessageOverlay],
) -> std::result::Result<Vec<ModelMessage>, SafePersistenceError> {
    let mut seen = BTreeSet::new();
    for overlay in overlays {
        if !seen.insert(overlay.durable_message_id()) {
            return Err(SafePersistenceError::OverlayInvariant {
                reason: format!(
                    "duplicate overlay for durable message {}",
                    overlay.durable_message_id()
                ),
            });
        }
        let matches = safe_messages
            .iter()
            .filter(|message| message.id == overlay.durable_message_id())
            .count();
        if matches != 1 {
            return Err(SafePersistenceError::OverlayInvariant {
                reason: format!(
                    "overlay for durable message {} matched {matches} safe messages",
                    overlay.durable_message_id()
                ),
            });
        }
        let Some(safe) = safe_messages
            .iter()
            .find(|message| message.id == overlay.durable_message_id())
        else {
            return Err(SafePersistenceError::OverlayInvariant {
                reason: format!(
                    "overlay for durable message {} disappeared during validation",
                    overlay.durable_message_id()
                ),
            });
        };
        if safe.role != overlay.exact_message().role {
            return Err(SafePersistenceError::OverlayInvariant {
                reason: format!(
                    "overlay role for durable message {} differs from safe message role",
                    overlay.durable_message_id()
                ),
            });
        }
    }

    Ok(safe_messages
        .iter()
        .map(|message| {
            overlays
                .iter()
                .find(|overlay| overlay.durable_message_id() == message.id)
                .map_or_else(
                    || message.clone(),
                    |overlay| overlay.exact_message().clone(),
                )
        })
        .collect())
}

/// Redacts sensitive URL/query material and secret-shaped JSON values from arbitrary text.
#[must_use]
pub fn safe_persistence_text(value: &str) -> String {
    let url_safe = project_text_urls("safe-projection", value, None, unix_time_ms(), false)
        .map(|(safe, _)| safe)
        .unwrap_or_else(|_| "[unsafe text projection failed]".to_owned());
    redact_secret_carriers(&url_safe)
}

fn validate_tool_call_identity(call: &ToolCall) -> std::result::Result<(), SafePersistenceError> {
    validate_tool_call_id(&call.id)?;
    validate_tool_call_name(&call.name)
}

pub(crate) fn validate_tool_call_id(value: &str) -> std::result::Result<(), SafePersistenceError> {
    validate_tool_call_identity_part("id", value, MAX_TOOL_CALL_ID_BYTES)
}

pub(crate) fn validate_tool_call_name(
    value: &str,
) -> std::result::Result<(), SafePersistenceError> {
    validate_tool_call_identity_part("name", value, MAX_TOOL_CALL_NAME_BYTES)
}

fn validate_tool_call_identity_part(
    field: &'static str,
    value: &str,
    limit: usize,
) -> std::result::Result<(), SafePersistenceError> {
    if value.len() > limit {
        return Err(SafePersistenceError::ToolCallIdentityTooLarge {
            field,
            observed_bytes: value.len(),
            limit_bytes: limit,
        });
    }
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
        || tool_identity_has_secret_marker(value)
        || safe_persistence_text(value) != value
    {
        return Err(SafePersistenceError::ToolCallIdentityUnsafe { field });
    }
    Ok(())
}

fn tool_identity_has_secret_marker(value: &str) -> bool {
    value
        .split(['_', '-', '.', ':'])
        .map(str::to_ascii_lowercase)
        .any(|segment| {
            matches!(
                segment.as_str(),
                "authorization"
                    | "bearer"
                    | "cookie"
                    | "credential"
                    | "password"
                    | "secret"
                    | "signature"
                    | "sig"
                    | "token"
                    | "apikey"
                    | "accesskey"
            )
        })
}

fn sanitize_json_value(value: &mut Value) {
    match value {
        Value::String(string) => *string = safe_persistence_text(string),
        Value::Array(values) => values.iter_mut().for_each(sanitize_json_value),
        Value::Object(object) => sanitize_json_object(object),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn sanitize_json_object(object: &mut Map<String, Value>) {
    for (key, value) in object {
        if secret_shaped_key(key) {
            *value = Value::String("[redacted]".to_owned());
        } else {
            sanitize_json_value(value);
        }
    }
}

fn secret_shaped_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "authorization",
        "cookie",
        "credential",
        "password",
        "secret",
        "signature",
        "sig",
        "token",
        "apikey",
        "accesskey",
    ]
    .iter()
    .any(|candidate| normalized == *candidate || normalized.ends_with(candidate))
}

fn redact_secret_carriers(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut redact_next = false;
    let mut cursor = 0usize;
    while cursor < value.len() {
        let is_whitespace = value[cursor..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace);
        let end = value[cursor..]
            .char_indices()
            .find_map(|(offset, character)| {
                (character.is_whitespace() != is_whitespace).then_some(cursor + offset)
            })
            .unwrap_or(value.len());
        let token = &value[cursor..end];
        cursor = end;
        if is_whitespace {
            output.push_str(token);
            continue;
        }
        if redact_next {
            output.push_str("[redacted]");
            redact_next = false;
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if [
            "--token",
            "--secret",
            "--password",
            "--api-key",
            "authorization:",
        ]
        .contains(&lower.as_str())
        {
            output.push_str(token);
            redact_next = true;
            continue;
        }
        let mut projected = None;
        for marker in [
            "token=",
            "secret=",
            "password=",
            "api_key=",
            "apikey=",
            "authorization=",
        ] {
            if let Some(index) = lower.find(marker) {
                let prefix_end = index + marker.len();
                projected = Some(format!("{}[redacted]", &token[..prefix_end]));
                break;
            }
        }
        output.push_str(projected.as_deref().unwrap_or(token));
    }
    output
}

fn project_text_urls(
    durable_message_id: &str,
    raw_text: &str,
    source_id_nonce: Option<&str>,
    issued_at_ms: u64,
    include_capability_labels: bool,
) -> Result<(String, Vec<UserUrlCapabilityRegistration>)> {
    let spans = url_spans(raw_text);
    if spans.is_empty() {
        return Ok((raw_text.to_owned(), Vec::new()));
    }
    let mut safe = String::with_capacity(raw_text.len());
    let mut cursor = 0usize;
    let mut registrations = Vec::with_capacity(spans.len());
    for (ordinal, (start, end)) in spans.into_iter().enumerate() {
        safe.push_str(&raw_text[cursor..start]);
        let raw_url = &raw_text[start..end];
        let canonical = canonical_web_url_persistence_projection(raw_url)
            .with_context(|| format!("failed to project URL at byte range {start}..{end}"))?;
        if include_capability_labels {
            let source_id = random_source_id(durable_message_id, source_id_nonce, ordinal);
            safe.push_str(&format!(
                "[web-source source_id={source_id} safe_url={}]",
                canonical.safe_display_url
            ));
            registrations.push(UserUrlCapabilityRegistration {
                source_id,
                durable_entry_id: durable_message_id.to_owned(),
                raw_canonical_url: canonical.raw_canonical_url,
                safe_display_url: canonical.safe_display_url,
                restart_policy: canonical.restart_policy,
                replayable_canonical_url: canonical.replayable_canonical_url,
                originating_call_id: None,
                provenance: WebUrlProvenanceKind::UserMessage,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(DEFAULT_WEB_URL_CAPABILITY_TTL_MS),
            });
        } else {
            safe.push_str(&canonical.safe_display_url);
        }
        cursor = end;
    }
    safe.push_str(&raw_text[cursor..]);
    Ok((safe, registrations))
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn validate_observed_url(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("observed URL scheme must be http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("observed URL must not contain userinfo");
    }
    let host = url.host_str().unwrap_or_default();
    if host.is_empty() || host.contains('%') {
        bail!("observed URL host is missing or contains a zone identifier");
    }
    Ok(())
}

fn safe_display_url(url: &Url, sensitive_path: bool) -> String {
    if sensitive_path {
        return format!("{}/[redacted]", url.origin().ascii_serialization());
    }
    let mut safe = url.clone();
    let had_query_or_fragment = safe.query().is_some() || safe.fragment().is_some();
    safe.set_query(None);
    safe.set_fragment(None);
    let mut value = safe.to_string();
    if had_query_or_fragment {
        value.push_str("?[redacted]");
    }
    value
}

fn url_has_sensitive_path(url: &Url) -> bool {
    percent_decode_for_detection(url.path())
        .split('/')
        .any(sensitive_path_segment)
}

fn sensitive_path_segment(segment: &str) -> bool {
    if segment.is_empty() {
        return false;
    }
    let lower = segment.to_ascii_lowercase();
    if [
        "token",
        "secret",
        "signature",
        "credential",
        "password",
        "api-key",
        "apikey",
        "access-key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return true;
    }
    let compact = segment
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        .collect::<Vec<_>>();
    compact.len() >= 24
        && compact.iter().any(u8::is_ascii_alphabetic)
        && compact.iter().any(u8::is_ascii_digit)
}

fn percent_decode_for_detection(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn random_source_id(
    durable_message_id: &str,
    source_id_nonce: Option<&str>,
    ordinal: usize,
) -> String {
    let id = match source_id_nonce {
        Some(nonce) => {
            let name = format!("{nonce}:{durable_message_id}:{ordinal}");
            Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
        }
        None => Uuid::new_v4(),
    };
    format!("src_{}", id.simple())
}

fn is_session_local_source_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("src_") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn url_spans(value: &str) -> Vec<(usize, usize)> {
    let bytes = value.as_bytes();
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let suffix = &value[cursor..];
        let http = suffix.find("http://");
        let https = suffix.find("https://");
        let relative = match (http, https) {
            (Some(left), Some(right)) => left.min(right),
            (Some(found), None) | (None, Some(found)) => found,
            (None, None) => break,
        };
        let start = cursor + relative;
        let mut end = start;
        while end < bytes.len() && !url_token_delimiter(bytes[end]) {
            end += 1;
        }
        while end > start && matches!(bytes[end - 1], b'.' | b',' | b'!' | b';') {
            end -= 1;
        }
        if end > start && Url::parse(&value[start..end]).is_ok() {
            spans.push((start, end));
        }
        cursor = end.max(start + 1);
    }
    spans
}

fn url_token_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'"' | b'\'' | b'<' | b'>' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'|'
        )
}

#[cfg(test)]
#[path = "tests/safe_persistence_tests.rs"]
mod tests;
