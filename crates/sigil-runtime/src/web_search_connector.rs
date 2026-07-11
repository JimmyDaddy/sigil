use std::sync::RwLock;

use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    ExternalSourceRecord, RunCancellationHandle, SecretRedactor, SecretString, WebQueryEgressClass,
    WebSearchFailureClass, is_unsafe_external_control, strip_terminal_control_sequences,
};
use sigil_mcp::McpSearchAdapterKind;
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const SEARCH_ID_MAX_BYTES: usize = 512;
const SEARCH_FINGERPRINT_BYTES: usize = 64;
pub const WEB_SEARCH_QUERY_MAX_CHARS: usize = 2_048;
pub const WEB_SEARCH_QUERY_MAX_BYTES: usize = 8 * 1_024;

#[async_trait]
pub trait WebSearchConnector: Send + Sync {
    fn identity(&self) -> WebSearchConnectorIdentity;

    async fn search(
        &self,
        request: WebSearchRequest,
    ) -> Result<WebSearchResponse, WebSearchConnectorError>;
}

pub struct WebSearchRequest {
    pub correlation_id: String,
    pub query: SecretString,
    pub query_chars: usize,
    pub query_bytes: usize,
    pub provenance: WebQueryEgressClass,
    pub max_results: u32,
    pub retrieved_at: String,
    pub cancellation: Option<RunCancellationHandle>,
}

impl std::fmt::Debug for WebSearchRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSearchRequest")
            .field("correlation_id", &self.correlation_id)
            .field("query", &"[redacted]")
            .field("query_chars", &self.query_chars)
            .field("query_bytes", &self.query_bytes)
            .field("provenance", &self.provenance)
            .field("max_results", &self.max_results)
            .field("retrieved_at", &self.retrieved_at)
            .field("cancellation", &self.cancellation.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchResponse {
    pub safe_model_content: String,
    pub sources: Vec<ExternalSourceRecord>,
    pub source_projection: SourceProjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceProjection {
    Structured {
        codec_id: String,
        valid_records: usize,
    },
    Unavailable {
        reason: SourceProjectionUnavailableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceProjectionUnavailableReason {
    ConnectorReturnedPlainText,
    GenericAdapterNoSourceContract,
    NoValidRecords,
    CodecFormatDrift,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchConnectorIdentity {
    pub origin: McpSearchBindingOrigin,
    pub safe_destination: String,
    pub server_identity_fingerprint: String,
    pub tool_schema_fingerprint: String,
    pub codec_id: Option<String>,
    pub disclosure_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchFailure {
    pub class: WebSearchFailureClass,
    pub retry_after_secs: Option<u64>,
    pub protocol_detail: Option<WebSearchProtocolFailureKind>,
}

impl WebSearchFailure {
    #[must_use]
    pub fn new(class: WebSearchFailureClass) -> Self {
        Self {
            class,
            retry_after_secs: None,
            protocol_detail: None,
        }
    }

    #[must_use]
    pub fn protocol(detail: WebSearchProtocolFailureKind) -> Self {
        Self {
            class: WebSearchFailureClass::ProtocolError,
            retry_after_secs: None,
            protocol_detail: Some(detail),
        }
    }

    pub fn validate(&self) -> Result<(), WebSearchConnectorError> {
        if (self.class == WebSearchFailureClass::ProtocolError) != self.protocol_detail.is_some() {
            return Err(WebSearchConnectorError::InvalidFailureContract);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSearchProtocolFailureKind {
    JsonRpcError { code: i64 },
    MalformedEnvelope,
    ResponseIdMismatch,
    UnsupportedProtocolVersion,
    InitializedNotificationRejected,
    InvalidSessionId,
    UnexpectedSessionId,
    MissingToolsCapability,
    InvalidPagination,
    MissingRequiredContent,
    UnexpectedContentType,
    UnexpectedHttpStatus { status: u16 },
}

#[derive(Debug, Error)]
pub enum WebSearchConnectorError {
    #[error("stable web search failed")]
    Failed(WebSearchFailure),
    #[error("stable web search failure contract is inconsistent")]
    InvalidFailureContract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpSearchBindingOrigin {
    UserConfigured,
    Bundled {
        profile_id: String,
        disclosure_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMcpSearchBinding {
    pub server_name: String,
    pub tool_name: String,
    pub origin: McpSearchBindingOrigin,
    pub root_run_id: String,
    pub config_epoch: u64,
}

impl PendingMcpSearchBinding {
    pub fn validate(&self) -> Result<(), McpSearchBindingRegistryError> {
        if !valid_id(&self.server_name)
            || !valid_id(&self.tool_name)
            || !valid_id(&self.root_run_id)
            || self.config_epoch == 0
            || !valid_origin(&self.origin)
        {
            return Err(McpSearchBindingRegistryError::InvalidBinding);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMcpSearchBinding {
    pub server_name: String,
    pub tool_name: String,
    pub origin: McpSearchBindingOrigin,
    pub adapter: McpSearchAdapterKind,
    pub safe_destination: String,
    pub server_identity_fingerprint: String,
    pub tool_schema_fingerprint: String,
    pub transport_fingerprint: String,
    pub live_header_fingerprint: String,
    pub source_policy_fingerprint: String,
    pub effective_policy_fingerprint: String,
    pub profile_config_proxy_fingerprint: String,
    pub root_run_id: String,
    pub config_epoch: u64,
}

impl PreparedMcpSearchBinding {
    pub fn validate(&self) -> Result<(), McpSearchBindingRegistryError> {
        if !valid_id(&self.server_name)
            || !valid_id(&self.tool_name)
            || !valid_origin(&self.origin)
            || !valid_destination(&self.safe_destination)
            || !valid_fingerprint(&self.server_identity_fingerprint)
            || !valid_fingerprint(&self.tool_schema_fingerprint)
            || !valid_fingerprint(&self.transport_fingerprint)
            || !valid_live_fingerprint(&self.live_header_fingerprint)
            || !valid_fingerprint(&self.source_policy_fingerprint)
            || !valid_fingerprint(&self.effective_policy_fingerprint)
            || !valid_fingerprint(&self.profile_config_proxy_fingerprint)
            || !valid_id(&self.root_run_id)
            || self.config_epoch == 0
        {
            return Err(McpSearchBindingRegistryError::InvalidBinding);
        }
        Ok(())
    }

    fn matches_pending(&self, pending: &PendingMcpSearchBinding) -> bool {
        self.server_name == pending.server_name
            && self.tool_name == pending.tool_name
            && self.origin == pending.origin
            && self.root_run_id == pending.root_run_id
            && self.config_epoch == pending.config_epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfiguredMcpSearchBindingState {
    Absent,
    PresentUnresolved {
        declaration: PendingMcpSearchBinding,
    },
    Eligible {
        binding: PreparedMcpSearchBinding,
    },
    Unavailable {
        declaration: PendingMcpSearchBinding,
        failure: WebSearchFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMcpSearchLease {
    revision: u64,
    pub binding: PreparedMcpSearchBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StableMcpRouteSelection {
    Bundled,
    ConfiguredPending,
    Configured(Box<PreparedMcpSearchLease>),
    ConfiguredUnavailable(WebSearchFailure),
    Unavailable,
}

#[derive(Debug)]
struct BindingSlot {
    revision: u64,
    state: ConfiguredMcpSearchBindingState,
}

#[derive(Debug)]
pub struct McpSearchBindingRegistry {
    inner: RwLock<BindingSlot>,
}

impl Default for McpSearchBindingRegistry {
    fn default() -> Self {
        Self {
            inner: RwLock::new(BindingSlot {
                revision: 0,
                state: ConfiguredMcpSearchBindingState::Absent,
            }),
        }
    }
}

impl McpSearchBindingRegistry {
    pub fn declare(
        &self,
        declaration: PendingMcpSearchBinding,
    ) -> Result<u64, McpSearchBindingRegistryError> {
        declaration.validate()?;
        let mut inner = self
            .inner
            .write()
            .map_err(|_| McpSearchBindingRegistryError::Poisoned)?;
        inner.revision = inner
            .revision
            .checked_add(1)
            .ok_or(McpSearchBindingRegistryError::RevisionExhausted)?;
        inner.state = ConfiguredMcpSearchBindingState::PresentUnresolved { declaration };
        Ok(inner.revision)
    }

    pub fn activate(
        &self,
        expected_revision: u64,
        activation: Result<PreparedMcpSearchBinding, WebSearchFailure>,
    ) -> Result<(), McpSearchBindingRegistryError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| McpSearchBindingRegistryError::Poisoned)?;
        if inner.revision != expected_revision {
            return Err(McpSearchBindingRegistryError::StaleRevision);
        }
        let ConfiguredMcpSearchBindingState::PresentUnresolved { declaration } = &inner.state
        else {
            return Err(McpSearchBindingRegistryError::InvalidTransition);
        };
        let declaration = declaration.clone();
        inner.state = match activation {
            Ok(binding) => {
                binding.validate()?;
                if !binding.matches_pending(&declaration) {
                    return Err(McpSearchBindingRegistryError::BindingMismatch);
                }
                ConfiguredMcpSearchBindingState::Eligible { binding }
            }
            Err(failure) => {
                failure
                    .validate()
                    .map_err(|_| McpSearchBindingRegistryError::InvalidBinding)?;
                ConfiguredMcpSearchBindingState::Unavailable {
                    declaration,
                    failure,
                }
            }
        };
        Ok(())
    }

    pub fn clear(&self) -> Result<(), McpSearchBindingRegistryError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| McpSearchBindingRegistryError::Poisoned)?;
        inner.revision = inner
            .revision
            .checked_add(1)
            .ok_or(McpSearchBindingRegistryError::RevisionExhausted)?;
        inner.state = ConfiguredMcpSearchBindingState::Absent;
        Ok(())
    }

    pub fn state(&self) -> Result<ConfiguredMcpSearchBindingState, McpSearchBindingRegistryError> {
        self.inner
            .read()
            .map(|inner| inner.state.clone())
            .map_err(|_| McpSearchBindingRegistryError::Poisoned)
    }

    pub fn select_auto(
        &self,
        bundled_enabled: bool,
    ) -> Result<StableMcpRouteSelection, McpSearchBindingRegistryError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| McpSearchBindingRegistryError::Poisoned)?;
        Ok(match &inner.state {
            ConfiguredMcpSearchBindingState::Absent if bundled_enabled => {
                StableMcpRouteSelection::Bundled
            }
            ConfiguredMcpSearchBindingState::Absent => StableMcpRouteSelection::Unavailable,
            ConfiguredMcpSearchBindingState::PresentUnresolved { .. } => {
                StableMcpRouteSelection::ConfiguredPending
            }
            ConfiguredMcpSearchBindingState::Eligible { binding } => {
                StableMcpRouteSelection::Configured(Box::new(PreparedMcpSearchLease {
                    revision: inner.revision,
                    binding: binding.clone(),
                }))
            }
            ConfiguredMcpSearchBindingState::Unavailable { failure, .. } => {
                StableMcpRouteSelection::ConfiguredUnavailable(failure.clone())
            }
        })
    }

    pub fn validate_lease(
        &self,
        lease: &PreparedMcpSearchLease,
    ) -> Result<(), McpSearchBindingRegistryError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| McpSearchBindingRegistryError::Poisoned)?;
        match &inner.state {
            ConfiguredMcpSearchBindingState::Eligible { binding }
                if inner.revision == lease.revision && binding == &lease.binding =>
            {
                Ok(())
            }
            _ => Err(McpSearchBindingRegistryError::StaleLease),
        }
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum McpSearchBindingRegistryError {
    #[error("stable MCP search binding is invalid")]
    InvalidBinding,
    #[error("stable MCP search binding does not match its declaration")]
    BindingMismatch,
    #[error("stable MCP search binding transition is invalid")]
    InvalidTransition,
    #[error("stable MCP search binding revision is stale")]
    StaleRevision,
    #[error("stable MCP search binding lease is stale")]
    StaleLease,
    #[error("stable MCP search binding revision is exhausted")]
    RevisionExhausted,
    #[error("stable MCP search binding lock is poisoned")]
    Poisoned,
}

pub struct NormalizedWebSearchQuery {
    pub query: SecretString,
    pub chars: usize,
    pub bytes: usize,
}

impl std::fmt::Debug for NormalizedWebSearchQuery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NormalizedWebSearchQuery")
            .field("query", &"[redacted]")
            .field("chars", &self.chars)
            .field("bytes", &self.bytes)
            .finish()
    }
}

pub fn normalize_web_search_query(
    raw: &str,
    redactor: &SecretRedactor,
    bundled_anonymous: bool,
) -> Result<NormalizedWebSearchQuery, WebSearchConnectorError> {
    let terminal_safe = strip_terminal_control_sequences(raw);
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in terminal_safe.nfc() {
        if is_unsafe_external_control(character) {
            pending_space = !normalized.is_empty();
            continue;
        }
        if character.is_whitespace() {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        normalized.push(character);
    }
    let chars = normalized.chars().count();
    let bytes = normalized.len();
    if normalized.is_empty()
        || chars > WEB_SEARCH_QUERY_MAX_CHARS
        || bytes > WEB_SEARCH_QUERY_MAX_BYTES
    {
        return Err(failure(WebSearchFailureClass::InvalidInput));
    }
    if redactor.text_contains_secret(&normalized) {
        return Err(failure(WebSearchFailureClass::SecretBlocked));
    }
    if bundled_anonymous && contains_high_confidence_personal_data(&normalized) {
        return Err(failure(WebSearchFailureClass::SensitivePersonalDataBlocked));
    }
    Ok(NormalizedWebSearchQuery {
        query: SecretString::new(normalized),
        chars,
        bytes,
    })
}

#[must_use]
pub fn generic_query_arguments(query: &SecretString) -> Value {
    json!({ "query": query.expose_secret() })
}

fn contains_high_confidence_personal_data(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let trimmed = token.trim_matches(|character: char| {
            matches!(character, ',' | '.' | ';' | ':' | '(' | ')' | '[' | ']')
        });
        looks_like_email(trimmed)
            || looks_like_ssn(trimmed)
            || looks_like_international_phone(trimmed)
            || looks_like_payment_card(trimmed)
    })
}

fn looks_like_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && domain.len() <= 253
        && domain.contains('.')
        && local.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
        })
        && domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn looks_like_ssn(value: &str) -> bool {
    value.len() == 11
        && value.as_bytes()[3] == b'-'
        && value.as_bytes()[6] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 3 | 6) || byte.is_ascii_digit())
}

fn looks_like_international_phone(value: &str) -> bool {
    if !value.starts_with('+') {
        return false;
    }
    let digits = value.bytes().filter(u8::is_ascii_digit).count();
    (10..=15).contains(&digits)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'(' | b')'))
}

fn looks_like_payment_card(value: &str) -> bool {
    let digits = value
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|byte| byte - b'0')
        .collect::<Vec<_>>();
    if !(13..=19).contains(&digits.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-'))
    {
        return false;
    }
    let checksum = digits
        .iter()
        .rev()
        .enumerate()
        .fold(0u32, |sum, (index, digit)| {
            let mut value = u32::from(*digit);
            if index % 2 == 1 {
                value *= 2;
                if value > 9 {
                    value -= 9;
                }
            }
            sum + value
        });
    checksum.is_multiple_of(10)
}

fn failure(class: WebSearchFailureClass) -> WebSearchConnectorError {
    WebSearchConnectorError::Failed(WebSearchFailure::new(class))
}

fn valid_origin(origin: &McpSearchBindingOrigin) -> bool {
    match origin {
        McpSearchBindingOrigin::UserConfigured => true,
        McpSearchBindingOrigin::Bundled {
            profile_id,
            disclosure_id,
        } => valid_id(profile_id) && valid_id(disclosure_id),
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= SEARCH_ID_MAX_BYTES
        && !value.chars().any(is_unsafe_external_control)
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == SEARCH_FINGERPRINT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_live_fingerprint(value: &str) -> bool {
    value
        .strip_prefix("hmac-sha256:")
        .is_some_and(valid_fingerprint)
}

fn valid_destination(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2_048
        && !value.contains(['?', '#', '@'])
        && !value.chars().any(is_unsafe_external_control)
}

#[cfg(test)]
#[path = "tests/web_search_connector_tests.rs"]
mod tests;
