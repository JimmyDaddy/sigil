use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    ApprovalMode, DurableAppendExpectation, DurableAppendPermit, DurableAppendRecordExpectation,
    DurableAuditBatch, DurableAuditError, DurableAuditRecord, DurableAuditWriter, DurableEventType,
    EventClass, JsonlSessionStore, SessionStreamRecord, safe_persistence_text,
};

const EGRESS_ID_MAX_BYTES: usize = 512;
const EGRESS_LABEL_MAX_BYTES: usize = 1024;
const EGRESS_DESTINATION_MAX_BYTES: usize = 2048;
const SHA256_HEX_BYTES: usize = 64;

/// Provider-neutral origin of a network-capable MCP binding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EgressBindingOrigin {
    UserConfigured,
    PluginDeclared,
    BundledReleaseProfile,
    LocalStdioBridge,
}

/// Safe route projection used by durable disclosure and authorization records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EgressNetworkRoute {
    Direct,
    ProxyRemote,
}

/// Whether one disclosure guards connection metadata or one exact logical query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EgressDisclosureKind {
    Transport,
    Query,
}

/// Data categories shown by a product-specific disclosure presenter.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum EgressDataCategory {
    SearchQuery,
    ConnectionMetadata,
    WorkspaceRootUri,
    InteractiveUserResponse,
}

/// Safe origin classification for a query. The exact query is never durable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebQueryEgressClass {
    UserProvided,
    ModelGenerated,
    ToolDerived,
}

/// Stable failure classes shared by durable outcomes and later connector adapters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchFailureClass {
    PolicyDenied,
    ApprovalRejected,
    SecretBlocked,
    SensitivePersonalDataBlocked,
    InvalidInput,
    ConfigurationInvalid,
    Cancelled,
    AuthenticationRequired,
    AuthenticationFailed,
    OAuthUnsupported,
    AccessDenied,
    RateLimited,
    SessionExpired,
    IdentityMismatch,
    SchemaDrift,
    ProtocolError,
    ToolExecutionFailed,
    TransportUnavailable,
    Timeout,
    ServiceUnavailable,
    UnexpectedResponse,
    BudgetExhausted,
    DisclosureFailed,
}

/// Hosted provider authorization scope. Exact provider wire names stay outside this contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostedAuthorizationScope {
    ProviderRequest,
}

/// Recovery-critical authorization appended before a hosted-enabled provider request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct HostedToolAuthorization {
    pub record_id: String,
    pub root_run_id: String,
    pub correlation_id: String,
    pub authorization_id: String,
    pub route_lease_id: String,
    pub hosted_request_fingerprint: String,
    pub provider_name: String,
    pub model_name: String,
    pub effect: ApprovalMode,
    pub scope: HostedAuthorizationScope,
}

/// Terminal state for one hosted authorization.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostedToolTerminalStatus {
    Observed,
    NotUsed,
    RequestFailed,
    Cancelled,
    Interrupted,
}

/// Exactly one terminal record for a prior hosted authorization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct HostedToolOutcome {
    pub record_id: String,
    pub root_run_id: String,
    pub correlation_id: String,
    pub authorization_id: String,
    pub hosted_request_fingerprint: String,
    pub status: HostedToolTerminalStatus,
}

/// Recovery-critical authorization appended before every MCP connect or reconnect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpTransportAuthorization {
    pub record_id: String,
    pub root_run_id: String,
    pub authorization_id: String,
    pub disclosure_id: String,
    pub binding_origin: EgressBindingOrigin,
    pub route_fingerprint: String,
    pub profile_config_proxy_fingerprint: String,
    pub route: EgressNetworkRoute,
    pub safe_logical_destination: String,
    pub safe_transport_destination: String,
}

/// Recovery-critical authorization appended before one built-in WebFetch hop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WebFetchTransportAuthorization {
    pub record_id: String,
    pub root_run_id: String,
    pub authorization_id: String,
    pub disclosure_id: String,
    pub route_fingerprint: String,
    pub profile_config_proxy_fingerprint: String,
    pub route: EgressNetworkRoute,
    pub safe_logical_destination: String,
    pub safe_transport_destination: String,
}

/// Immutable, secret-safe disclosure pending presentation by the active product surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PreEgressDisclosure {
    kind: EgressDisclosureKind,
    correlation_id: Option<String>,
    disclosure_id: String,
    surface: String,
    display_name: String,
    route_fingerprint: String,
    profile_config_proxy_fingerprint: String,
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: EgressNetworkRoute,
    data_categories: Vec<EgressDataCategory>,
    disclosure_content_sha256: String,
}

impl PreEgressDisclosure {
    /// Creates a canonical disclosure and computes its content digest from safe fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: EgressDisclosureKind,
        correlation_id: Option<String>,
        disclosure_id: impl Into<String>,
        surface: impl Into<String>,
        display_name: impl Into<String>,
        route_fingerprint: impl Into<String>,
        profile_config_proxy_fingerprint: impl Into<String>,
        safe_logical_destination: impl Into<String>,
        safe_transport_destination: impl Into<String>,
        route: EgressNetworkRoute,
        mut data_categories: Vec<EgressDataCategory>,
    ) -> Result<Self, EgressAuditError> {
        data_categories.sort_unstable();
        data_categories.dedup();
        let mut disclosure = Self {
            kind,
            correlation_id,
            disclosure_id: disclosure_id.into(),
            surface: surface.into(),
            display_name: display_name.into(),
            route_fingerprint: route_fingerprint.into(),
            profile_config_proxy_fingerprint: profile_config_proxy_fingerprint.into(),
            safe_logical_destination: safe_logical_destination.into(),
            safe_transport_destination: safe_transport_destination.into(),
            route,
            data_categories,
            disclosure_content_sha256: String::new(),
        };
        disclosure.validate_without_digest()?;
        disclosure.disclosure_content_sha256 = disclosure.compute_content_digest()?;
        disclosure.validate()?;
        Ok(disclosure)
    }

    #[must_use]
    pub fn kind(&self) -> EgressDisclosureKind {
        self.kind
    }

    #[must_use]
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    #[must_use]
    pub fn disclosure_id(&self) -> &str {
        &self.disclosure_id
    }

    /// Returns the product surface that will present this disclosure.
    #[must_use]
    pub fn surface(&self) -> &str {
        &self.surface
    }

    /// Returns the safe, user-facing destination label for this disclosure.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub fn route_fingerprint(&self) -> &str {
        &self.route_fingerprint
    }

    #[must_use]
    pub fn profile_config_proxy_fingerprint(&self) -> &str {
        &self.profile_config_proxy_fingerprint
    }

    #[must_use]
    pub fn safe_logical_destination(&self) -> &str {
        &self.safe_logical_destination
    }

    #[must_use]
    pub fn safe_transport_destination(&self) -> &str {
        &self.safe_transport_destination
    }

    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.disclosure_content_sha256
    }

    #[must_use]
    pub fn data_categories(&self) -> &[EgressDataCategory] {
        &self.data_categories
    }

    #[must_use]
    pub fn route(&self) -> EgressNetworkRoute {
        self.route
    }

    /// Creates a receipt only after the presenter has completed its sink-specific write/flush.
    pub fn presentation_receipt(
        &self,
        sink_fingerprint: impl Into<String>,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        let sink_fingerprint = sink_fingerprint.into();
        validate_safe_identity("sink_fingerprint", &sink_fingerprint)
            .map_err(|_| DisclosurePresentationError::InvalidSinkFingerprint)?;
        Ok(DisclosurePresentationReceipt {
            kind: self.kind,
            correlation_id: self.correlation_id.clone(),
            disclosure_id: self.disclosure_id.clone(),
            route_fingerprint: self.route_fingerprint.clone(),
            profile_config_proxy_fingerprint: self.profile_config_proxy_fingerprint.clone(),
            safe_logical_destination: self.safe_logical_destination.clone(),
            safe_transport_destination: self.safe_transport_destination.clone(),
            route: self.route,
            disclosure_content_sha256: self.disclosure_content_sha256.clone(),
            sink_fingerprint,
        })
    }

    fn validate_without_digest(&self) -> Result<(), EgressAuditError> {
        validate_safe_identity("disclosure_id", &self.disclosure_id)?;
        if let Some(correlation_id) = self.correlation_id.as_deref() {
            validate_safe_identity("correlation_id", correlation_id)?;
        }
        match (self.kind, self.correlation_id.as_deref()) {
            (EgressDisclosureKind::Transport, None) | (EgressDisclosureKind::Query, Some(_)) => {}
            _ => {
                return Err(EgressAuditError::InvalidRecord(
                    "transport disclosure must omit correlation and query disclosure must bind it"
                        .to_owned(),
                ));
            }
        }
        validate_safe_label("surface", &self.surface)?;
        validate_safe_label("display_name", &self.display_name)?;
        validate_safe_identity("route_fingerprint", &self.route_fingerprint)?;
        validate_safe_identity(
            "profile_config_proxy_fingerprint",
            &self.profile_config_proxy_fingerprint,
        )?;
        validate_safe_destination("safe_logical_destination", &self.safe_logical_destination)?;
        validate_safe_destination(
            "safe_transport_destination",
            &self.safe_transport_destination,
        )?;
        if self.data_categories.is_empty() {
            return Err(EgressAuditError::InvalidRecord(
                "disclosure must contain at least one data category".to_owned(),
            ));
        }
        if self.kind == EgressDisclosureKind::Query
            && !self
                .data_categories
                .contains(&EgressDataCategory::SearchQuery)
        {
            return Err(EgressAuditError::InvalidRecord(
                "query disclosure must include search_query".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), EgressAuditError> {
        self.validate_without_digest()?;
        validate_sha256("disclosure_content_sha256", &self.disclosure_content_sha256)?;
        if self.compute_content_digest()? != self.disclosure_content_sha256 {
            return Err(EgressAuditError::InvalidRecord(
                "disclosure content digest does not match its safe fields".to_owned(),
            ));
        }
        Ok(())
    }

    fn compute_content_digest(&self) -> Result<String, EgressAuditError> {
        let value = serde_json::json!({
            "kind": self.kind,
            "correlation_id": self.correlation_id,
            "disclosure_id": self.disclosure_id,
            "surface": self.surface,
            "display_name": self.display_name,
            "route_fingerprint": self.route_fingerprint,
            "profile_config_proxy_fingerprint": self.profile_config_proxy_fingerprint,
            "safe_logical_destination": self.safe_logical_destination,
            "safe_transport_destination": self.safe_transport_destination,
            "route": self.route,
            "data_categories": self.data_categories,
        });
        let bytes = serde_json::to_vec(&value)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

/// One-shot receipt returned by a concrete or deterministic fake presenter.
#[derive(Debug)]
pub struct DisclosurePresentationReceipt {
    kind: EgressDisclosureKind,
    correlation_id: Option<String>,
    disclosure_id: String,
    route_fingerprint: String,
    profile_config_proxy_fingerprint: String,
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: EgressNetworkRoute,
    disclosure_content_sha256: String,
    sink_fingerprint: String,
}

impl DisclosurePresentationReceipt {
    /// Returns the disclosure kind bound to this one-shot receipt.
    #[must_use]
    pub fn kind(&self) -> EgressDisclosureKind {
        self.kind
    }

    /// Returns the optional query correlation bound to this receipt.
    #[must_use]
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    /// Returns the disclosure identity bound to this receipt.
    #[must_use]
    pub fn disclosure_id(&self) -> &str {
        &self.disclosure_id
    }

    /// Returns the product sink identity that completed presentation.
    #[must_use]
    pub fn sink_fingerprint(&self) -> &str {
        &self.sink_fingerprint
    }
}

/// Durable proof that the selected sink completed the disclosure action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EgressDisclosurePresented {
    pub record_id: String,
    pub kind: EgressDisclosureKind,
    pub correlation_id: Option<String>,
    pub disclosure_id: String,
    pub route_fingerprint: String,
    pub profile_config_proxy_fingerprint: String,
    pub safe_logical_destination: String,
    pub safe_transport_destination: String,
    pub route: EgressNetworkRoute,
    pub disclosure_content_sha256: String,
    pub sink_fingerprint: String,
}

/// Presenter failures are terminal and never grant a network permission.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DisclosurePresentationError {
    #[error("egress disclosure sink is closed")]
    SinkClosed,
    #[error("egress disclosure render failed")]
    RenderFailed,
    #[error("egress disclosure write failed")]
    WriteFailed,
    #[error("egress disclosure flush failed")]
    FlushFailed,
    #[error("egress disclosure sink fingerprint is invalid")]
    InvalidSinkFingerprint,
}

/// Product-specific presentation sink. A receipt is not an approval or proof of human reading.
#[async_trait]
pub trait EgressDisclosurePresenter: Send + Sync {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError>;
}

/// Strictly validates a one-shot receipt and converts it into the durable presentation record.
pub fn validate_disclosure_receipt(
    pending: &PreEgressDisclosure,
    receipt: DisclosurePresentationReceipt,
) -> Result<EgressDisclosurePresented, EgressAuditError> {
    pending.validate()?;
    let matched = receipt.kind == pending.kind
        && receipt.correlation_id == pending.correlation_id
        && receipt.disclosure_id == pending.disclosure_id
        && receipt.route_fingerprint == pending.route_fingerprint
        && receipt.profile_config_proxy_fingerprint == pending.profile_config_proxy_fingerprint
        && receipt.safe_logical_destination == pending.safe_logical_destination
        && receipt.safe_transport_destination == pending.safe_transport_destination
        && receipt.route == pending.route
        && receipt.disclosure_content_sha256 == pending.disclosure_content_sha256;
    if !matched {
        return Err(EgressAuditError::ReceiptMismatch);
    }
    validate_safe_identity("sink_fingerprint", &receipt.sink_fingerprint)?;
    let presented = EgressDisclosurePresented {
        record_id: format!("egress-presented-{}", Uuid::new_v4()),
        kind: receipt.kind,
        correlation_id: receipt.correlation_id,
        disclosure_id: receipt.disclosure_id,
        route_fingerprint: receipt.route_fingerprint,
        profile_config_proxy_fingerprint: receipt.profile_config_proxy_fingerprint,
        safe_logical_destination: receipt.safe_logical_destination,
        safe_transport_destination: receipt.safe_transport_destination,
        route: receipt.route,
        disclosure_content_sha256: receipt.disclosure_content_sha256,
        sink_fingerprint: receipt.sink_fingerprint,
    };
    presented.validate()?;
    Ok(presented)
}

/// Recovery-critical marker appended immediately before query body bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QueryEgressStarted {
    pub record_id: String,
    pub root_run_id: String,
    pub correlation_id: String,
    pub route_lease_id: String,
    pub route_fingerprint: String,
    pub query_chars: usize,
    pub query_bytes: usize,
    pub egress_class: WebQueryEgressClass,
}

/// Unique terminal status for a started query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryEgressTerminalStatus {
    Completed,
    Failed,
    RateLimited,
    Cancelled,
    Interrupted,
}

/// Exactly one terminal record for a prior query start.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QueryEgressOutcome {
    pub record_id: String,
    pub root_run_id: String,
    pub correlation_id: String,
    pub route_fingerprint: String,
    pub status: QueryEgressTerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<WebSearchFailureClass>,
}

impl QueryEgressOutcome {
    #[must_use]
    pub fn interrupted(started: &QueryEgressStarted) -> Self {
        Self {
            record_id: format!("query-outcome-{}-interrupted", started.correlation_id),
            root_run_id: started.root_run_id.clone(),
            correlation_id: started.correlation_id.clone(),
            route_fingerprint: started.route_fingerprint.clone(),
            status: QueryEgressTerminalStatus::Interrupted,
            error_class: None,
        }
    }
}

/// Typed failure from durable egress records and receipt validation.
#[derive(Debug, Error)]
pub enum EgressAuditError {
    #[error(transparent)]
    Durable(#[from] DurableAuditError),
    #[error("invalid durable egress record: {0}")]
    InvalidRecord(String),
    #[error("egress disclosure receipt does not match the pending disclosure")]
    ReceiptMismatch,
    #[error("durable egress lifecycle conflict: {0}")]
    LifecycleConflict(String),
    #[error("failed to encode or decode durable egress record")]
    Serialization(#[from] serde_json::Error),
    #[error("failed to read or append durable egress lifecycle")]
    Store(#[from] anyhow::Error),
}

/// Store-backed recorder used by runtime pre-egress ordering and recovery.
#[derive(Debug, Clone)]
pub struct EgressAuditRecorder {
    store: JsonlSessionStore,
}

impl EgressAuditRecorder {
    pub(crate) fn new(store: JsonlSessionStore) -> Self {
        Self { store }
    }

    pub fn append_hosted_authorization(
        &self,
        entry: &HostedToolAuthorization,
    ) -> Result<DurableAppendPermit, EgressAuditError> {
        entry.validate()?;
        let expected = entry.clone();
        self.append_barrier_if(
            DurableEventType::HostedToolAuthorization,
            entry,
            &entry.record_id,
            Some(entry.correlation_id.clone()),
            Some(entry.authorization_id.clone()),
            move |records| validate_new_hosted_authorization(records, &expected),
        )
    }

    pub fn append_transport_authorization(
        &self,
        entry: &McpTransportAuthorization,
    ) -> Result<DurableAppendPermit, EgressAuditError> {
        entry.validate()?;
        let expected = entry.clone();
        self.append_barrier_if(
            DurableEventType::McpTransportAuthorization,
            entry,
            &entry.record_id,
            None,
            Some(entry.authorization_id.clone()),
            move |records| validate_new_transport_authorization(records, &expected),
        )
    }

    pub fn append_webfetch_transport_authorization(
        &self,
        entry: &WebFetchTransportAuthorization,
    ) -> Result<DurableAppendPermit, EgressAuditError> {
        entry.validate()?;
        let expected = entry.clone();
        self.append_barrier_if(
            DurableEventType::WebFetchTransportAuthorization,
            entry,
            &entry.record_id,
            None,
            Some(entry.authorization_id.clone()),
            move |records| validate_new_webfetch_transport_authorization(records, &expected),
        )
    }

    pub fn append_disclosure_presented(
        &self,
        entry: &EgressDisclosurePresented,
    ) -> Result<DurableAppendPermit, EgressAuditError> {
        entry.validate()?;
        let expected = entry.clone();
        self.append_barrier_if(
            DurableEventType::EgressDisclosurePresented,
            entry,
            &entry.record_id,
            entry.correlation_id.clone(),
            None,
            move |records| validate_new_disclosure_presented(records, &expected),
        )
    }

    pub fn append_query_started(
        &self,
        entry: &QueryEgressStarted,
    ) -> Result<DurableAppendPermit, EgressAuditError> {
        entry.validate()?;
        let expected = entry.clone();
        self.append_barrier_if(
            DurableEventType::QueryEgressStarted,
            entry,
            &entry.record_id,
            Some(entry.correlation_id.clone()),
            None,
            move |records| validate_new_query_start(records, &expected),
        )
    }

    pub fn append_hosted_outcome(
        &self,
        entry: &HostedToolOutcome,
    ) -> Result<bool, EgressAuditError> {
        entry.validate()?;
        let value = serde_json::to_value(entry)?;
        let expected = entry.clone();
        Ok(self.store.append_event_if(
            DurableEventType::HostedToolOutcome,
            EventClass::Critical,
            value,
            move |records| validate_new_hosted_outcome(records, &expected),
        )?)
    }

    pub fn append_query_outcome(
        &self,
        entry: &QueryEgressOutcome,
    ) -> Result<bool, EgressAuditError> {
        entry.validate()?;
        let value = serde_json::to_value(entry)?;
        let expected = entry.clone();
        Ok(self.store.append_event_if(
            DurableEventType::QueryEgressOutcome,
            EventClass::Critical,
            value,
            move |records| validate_new_query_outcome(records, &expected),
        )?)
    }

    /// Appends Interrupted for every authorization/start left without a terminal record.
    /// Repeated recovery is idempotent and never replays a provider request or query.
    pub fn reconcile_interrupted(&self) -> Result<usize, EgressAuditError> {
        let records = self.store.read_event_records_writer()?;
        let lifecycle = egress_records_from_stream(&records)?;
        let mut appended = 0usize;
        for authorization in lifecycle.hosted_authorizations.values() {
            if !lifecycle
                .hosted_outcomes
                .contains_key(&authorization.authorization_id)
            {
                let outcome = HostedToolOutcome {
                    record_id: format!(
                        "hosted-outcome-{}-interrupted",
                        authorization.authorization_id
                    ),
                    root_run_id: authorization.root_run_id.clone(),
                    correlation_id: authorization.correlation_id.clone(),
                    authorization_id: authorization.authorization_id.clone(),
                    hosted_request_fingerprint: authorization.hosted_request_fingerprint.clone(),
                    status: HostedToolTerminalStatus::Interrupted,
                };
                appended += usize::from(self.append_hosted_outcome(&outcome)?);
            }
        }
        for started in lifecycle.query_starts.values() {
            if !lifecycle
                .query_outcomes
                .contains_key(&started.correlation_id)
            {
                appended += usize::from(
                    self.append_query_outcome(&QueryEgressOutcome::interrupted(started))?,
                );
            }
        }
        Ok(appended)
    }

    fn append_barrier_if<T, F>(
        &self,
        event_type: DurableEventType,
        entry: &T,
        record_id: &str,
        correlation_id: Option<String>,
        authorization_id: Option<String>,
        should_append: F,
    ) -> Result<DurableAppendPermit, EgressAuditError>
    where
        T: Serialize,
        F: FnOnce(&[SessionStreamRecord]) -> anyhow::Result<bool>,
    {
        let mut record = DurableAuditRecord::new(
            event_type,
            serde_json::to_value(entry)?,
            record_id.to_owned(),
            correlation_id.clone(),
        )?;
        if let Some(authorization_id) = authorization_id.as_deref() {
            record = record.with_authorization_id(authorization_id.to_owned())?;
        }
        let batch = DurableAuditBatch::new(record_id.to_owned(), vec![record])?;
        let receipt = self
            .store
            .append_audit_batch_if(batch, should_append)?
            .ok_or_else(|| {
                EgressAuditError::LifecycleConflict(
                    "protected egress record already exists".to_owned(),
                )
            })?;
        let mut expectation =
            DurableAppendRecordExpectation::new(event_type, record_id.to_owned(), correlation_id)?;
        if let Some(authorization_id) = authorization_id {
            expectation = expectation.with_authorization_id(authorization_id)?;
        }
        let expectation = DurableAppendExpectation::new(
            receipt.session_id().to_owned(),
            receipt.batch_id().to_owned(),
            vec![expectation],
        )?;
        Ok(DurableAuditWriter::validate_and_consume(
            &self.store,
            receipt,
            expectation,
        )?)
    }
}

impl HostedToolAuthorization {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_common_ids(
            &self.record_id,
            &self.root_run_id,
            Some(&self.correlation_id),
        )?;
        validate_safe_identity("authorization_id", &self.authorization_id)?;
        validate_safe_identity("route_lease_id", &self.route_lease_id)?;
        validate_safe_identity(
            "hosted_request_fingerprint",
            &self.hosted_request_fingerprint,
        )?;
        validate_safe_label("provider_name", &self.provider_name)?;
        validate_safe_label("model_name", &self.model_name)?;
        if self.effect != ApprovalMode::Allow {
            return Err(EgressAuditError::InvalidRecord(
                "hosted authorization must record an effective allow decision".to_owned(),
            ));
        }
        Ok(())
    }
}

impl HostedToolOutcome {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_common_ids(
            &self.record_id,
            &self.root_run_id,
            Some(&self.correlation_id),
        )?;
        validate_safe_identity("authorization_id", &self.authorization_id)?;
        validate_safe_identity(
            "hosted_request_fingerprint",
            &self.hosted_request_fingerprint,
        )
    }
}

impl McpTransportAuthorization {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_common_ids(&self.record_id, &self.root_run_id, None)?;
        validate_safe_identity("authorization_id", &self.authorization_id)?;
        validate_safe_identity("disclosure_id", &self.disclosure_id)?;
        validate_safe_identity("route_fingerprint", &self.route_fingerprint)?;
        validate_safe_identity(
            "profile_config_proxy_fingerprint",
            &self.profile_config_proxy_fingerprint,
        )?;
        validate_safe_destination("safe_logical_destination", &self.safe_logical_destination)?;
        validate_safe_destination(
            "safe_transport_destination",
            &self.safe_transport_destination,
        )
    }
}

impl WebFetchTransportAuthorization {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_common_ids(&self.record_id, &self.root_run_id, None)?;
        validate_safe_identity("authorization_id", &self.authorization_id)?;
        validate_safe_identity("disclosure_id", &self.disclosure_id)?;
        validate_safe_identity("route_fingerprint", &self.route_fingerprint)?;
        validate_safe_identity(
            "profile_config_proxy_fingerprint",
            &self.profile_config_proxy_fingerprint,
        )?;
        validate_safe_destination("safe_logical_destination", &self.safe_logical_destination)?;
        validate_safe_destination(
            "safe_transport_destination",
            &self.safe_transport_destination,
        )
    }
}

impl EgressDisclosurePresented {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_safe_identity("record_id", &self.record_id)?;
        validate_safe_identity("disclosure_id", &self.disclosure_id)?;
        if let Some(correlation_id) = self.correlation_id.as_deref() {
            validate_safe_identity("correlation_id", correlation_id)?;
        }
        match (self.kind, self.correlation_id.as_deref()) {
            (EgressDisclosureKind::Transport, None) | (EgressDisclosureKind::Query, Some(_)) => {}
            _ => {
                return Err(EgressAuditError::InvalidRecord(
                    "presented disclosure kind and correlation disagree".to_owned(),
                ));
            }
        }
        validate_safe_identity("route_fingerprint", &self.route_fingerprint)?;
        validate_safe_identity(
            "profile_config_proxy_fingerprint",
            &self.profile_config_proxy_fingerprint,
        )?;
        validate_safe_destination("safe_logical_destination", &self.safe_logical_destination)?;
        validate_safe_destination(
            "safe_transport_destination",
            &self.safe_transport_destination,
        )?;
        validate_sha256("disclosure_content_sha256", &self.disclosure_content_sha256)?;
        validate_safe_identity("sink_fingerprint", &self.sink_fingerprint)
    }
}

impl QueryEgressStarted {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_common_ids(
            &self.record_id,
            &self.root_run_id,
            Some(&self.correlation_id),
        )?;
        validate_safe_identity("route_lease_id", &self.route_lease_id)?;
        validate_safe_identity("route_fingerprint", &self.route_fingerprint)?;
        if self.query_chars == 0 || self.query_bytes == 0 || self.query_chars > self.query_bytes {
            return Err(EgressAuditError::InvalidRecord(
                "query size metadata must describe a non-empty UTF-8 query".to_owned(),
            ));
        }
        Ok(())
    }
}

impl QueryEgressOutcome {
    pub(crate) fn validate(&self) -> Result<(), EgressAuditError> {
        validate_common_ids(
            &self.record_id,
            &self.root_run_id,
            Some(&self.correlation_id),
        )?;
        validate_safe_identity("route_fingerprint", &self.route_fingerprint)?;
        let error_shape_valid = match self.status {
            QueryEgressTerminalStatus::Completed
            | QueryEgressTerminalStatus::Cancelled
            | QueryEgressTerminalStatus::Interrupted => self.error_class.is_none(),
            QueryEgressTerminalStatus::Failed | QueryEgressTerminalStatus::RateLimited => {
                self.error_class.is_some()
            }
        };
        if !error_shape_valid
            || (self.status == QueryEgressTerminalStatus::RateLimited
                && self.error_class != Some(WebSearchFailureClass::RateLimited))
        {
            return Err(EgressAuditError::InvalidRecord(
                "query terminal status and error class disagree".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct EgressLifecycleProjection {
    hosted_authorizations: BTreeMap<String, HostedToolAuthorization>,
    hosted_outcomes: BTreeMap<String, HostedToolOutcome>,
    transport_authorizations: BTreeMap<String, McpTransportAuthorization>,
    webfetch_transport_authorizations: BTreeMap<String, WebFetchTransportAuthorization>,
    disclosures_presented: BTreeMap<String, EgressDisclosurePresented>,
    query_starts: BTreeMap<String, QueryEgressStarted>,
    query_outcomes: BTreeMap<String, QueryEgressOutcome>,
}

fn egress_records_from_stream(
    records: &[SessionStreamRecord],
) -> Result<EgressLifecycleProjection, EgressAuditError> {
    let mut projection = EgressLifecycleProjection::default();
    for record in records {
        let SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        match event.event_kind() {
            Some(DurableEventType::HostedToolAuthorization) => {
                let entry: HostedToolAuthorization = serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .hosted_authorizations
                    .insert(entry.authorization_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate hosted authorization id".to_owned(),
                    ));
                }
            }
            Some(DurableEventType::HostedToolOutcome) => {
                let entry: HostedToolOutcome = serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .hosted_outcomes
                    .insert(entry.authorization_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate hosted terminal outcome".to_owned(),
                    ));
                }
            }
            Some(DurableEventType::McpTransportAuthorization) => {
                let entry: McpTransportAuthorization =
                    serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .transport_authorizations
                    .insert(entry.authorization_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate transport authorization id".to_owned(),
                    ));
                }
            }
            Some(DurableEventType::WebFetchTransportAuthorization) => {
                let entry: WebFetchTransportAuthorization =
                    serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .webfetch_transport_authorizations
                    .insert(entry.authorization_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate webfetch transport authorization id".to_owned(),
                    ));
                }
            }
            Some(DurableEventType::EgressDisclosurePresented) => {
                let entry: EgressDisclosurePresented =
                    serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .disclosures_presented
                    .insert(entry.disclosure_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate disclosure presentation id".to_owned(),
                    ));
                }
            }
            Some(DurableEventType::QueryEgressStarted) => {
                let entry: QueryEgressStarted = serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .query_starts
                    .insert(entry.correlation_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate query start correlation".to_owned(),
                    ));
                }
            }
            Some(DurableEventType::QueryEgressOutcome) => {
                let entry: QueryEgressOutcome = serde_json::from_value(event.payload.clone())?;
                entry.validate()?;
                if projection
                    .query_outcomes
                    .insert(entry.correlation_id.clone(), entry)
                    .is_some()
                {
                    return Err(EgressAuditError::LifecycleConflict(
                        "duplicate query terminal outcome".to_owned(),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(projection)
}

fn validate_new_hosted_authorization(
    records: &[SessionStreamRecord],
    expected: &HostedToolAuthorization,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    if projection
        .hosted_authorizations
        .contains_key(&expected.authorization_id)
        || projection
            .hosted_authorizations
            .values()
            .any(|entry| entry.correlation_id == expected.correlation_id)
    {
        anyhow::bail!("hosted authorization or correlation is already durable");
    }
    Ok(true)
}

fn validate_new_transport_authorization(
    records: &[SessionStreamRecord],
    expected: &McpTransportAuthorization,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    if projection
        .transport_authorizations
        .contains_key(&expected.authorization_id)
    {
        anyhow::bail!("transport authorization id is already durable");
    }
    Ok(true)
}

fn validate_new_webfetch_transport_authorization(
    records: &[SessionStreamRecord],
    expected: &WebFetchTransportAuthorization,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    if projection
        .webfetch_transport_authorizations
        .contains_key(&expected.authorization_id)
    {
        anyhow::bail!("webfetch transport authorization id is already durable");
    }
    Ok(true)
}

fn validate_new_disclosure_presented(
    records: &[SessionStreamRecord],
    expected: &EgressDisclosurePresented,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    if projection
        .disclosures_presented
        .contains_key(&expected.disclosure_id)
    {
        anyhow::bail!("disclosure presentation id is already durable");
    }
    Ok(true)
}

fn validate_new_query_start(
    records: &[SessionStreamRecord],
    expected: &QueryEgressStarted,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    if projection
        .query_starts
        .contains_key(&expected.correlation_id)
        || projection
            .query_outcomes
            .contains_key(&expected.correlation_id)
    {
        anyhow::bail!("query correlation already has a durable lifecycle");
    }
    Ok(true)
}

fn validate_new_hosted_outcome(
    records: &[SessionStreamRecord],
    expected: &HostedToolOutcome,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    let authorization = projection
        .hosted_authorizations
        .get(&expected.authorization_id)
        .ok_or_else(|| anyhow::anyhow!("hosted outcome requires a durable authorization"))?;
    if authorization.root_run_id != expected.root_run_id
        || authorization.correlation_id != expected.correlation_id
        || authorization.hosted_request_fingerprint != expected.hosted_request_fingerprint
    {
        anyhow::bail!("hosted outcome does not match its durable authorization");
    }
    match projection.hosted_outcomes.get(&expected.authorization_id) {
        None => Ok(true),
        Some(existing) if existing == expected => Ok(false),
        Some(_) => anyhow::bail!("hosted authorization already has a different terminal outcome"),
    }
}

fn validate_new_query_outcome(
    records: &[SessionStreamRecord],
    expected: &QueryEgressOutcome,
) -> anyhow::Result<bool> {
    let projection = egress_records_from_stream(records).map_err(anyhow::Error::from)?;
    let started = projection
        .query_starts
        .get(&expected.correlation_id)
        .ok_or_else(|| anyhow::anyhow!("query outcome requires a durable start"))?;
    if started.root_run_id != expected.root_run_id
        || started.route_fingerprint != expected.route_fingerprint
    {
        anyhow::bail!("query outcome does not match its durable start");
    }
    match projection.query_outcomes.get(&expected.correlation_id) {
        None => Ok(true),
        Some(existing) if existing == expected => Ok(false),
        Some(_) => anyhow::bail!("query already has a different terminal outcome"),
    }
}

fn validate_common_ids(
    record_id: &str,
    root_run_id: &str,
    correlation_id: Option<&str>,
) -> Result<(), EgressAuditError> {
    validate_safe_identity("record_id", record_id)?;
    validate_safe_identity("root_run_id", root_run_id)?;
    if let Some(correlation_id) = correlation_id {
        validate_safe_identity("correlation_id", correlation_id)?;
    }
    Ok(())
}

fn validate_safe_identity(field: &str, value: &str) -> Result<(), EgressAuditError> {
    if value.is_empty()
        || value.len() > EGRESS_ID_MAX_BYTES
        || value.chars().any(char::is_control)
        || safe_persistence_text(value) != value
    {
        return Err(EgressAuditError::InvalidRecord(format!(
            "{field} must be non-empty, bounded, control-free safe text"
        )));
    }
    Ok(())
}

fn validate_safe_label(field: &str, value: &str) -> Result<(), EgressAuditError> {
    if value.is_empty()
        || value.len() > EGRESS_LABEL_MAX_BYTES
        || value.chars().any(char::is_control)
        || safe_persistence_text(value) != value
    {
        return Err(EgressAuditError::InvalidRecord(format!(
            "{field} must be bounded safe text"
        )));
    }
    Ok(())
}

fn validate_safe_destination(field: &str, value: &str) -> Result<(), EgressAuditError> {
    if value.is_empty()
        || value.len() > EGRESS_DESTINATION_MAX_BYTES
        || value.chars().any(char::is_control)
        || safe_persistence_text(value) != value
    {
        return Err(EgressAuditError::InvalidRecord(format!(
            "{field} must be bounded safe text"
        )));
    }
    let parsed = Url::parse(value).map_err(|_| {
        EgressAuditError::InvalidRecord(format!("{field} must be an absolute safe URL"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(EgressAuditError::InvalidRecord(format!(
            "{field} must contain only scheme, host and optional port"
        )));
    }
    Ok(())
}

fn validate_sha256(field: &str, value: &str) -> Result<(), EgressAuditError> {
    if value.len() != SHA256_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(EgressAuditError::InvalidRecord(format!(
            "{field} must be 64 lowercase hexadecimal bytes"
        )));
    }
    Ok(())
}

/// Shared presenter handle used by runtime coordinators without choosing a concrete surface.
pub type SharedEgressDisclosurePresenter = Arc<dyn EgressDisclosurePresenter>;

#[cfg(test)]
#[path = "tests/egress_audit_tests.rs"]
mod tests;
