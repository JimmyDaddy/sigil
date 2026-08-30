use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::*;
use crate::projection_apply_decision;

/// Current durable schema for recovery of a possibly-applied external effect.
pub const EFFECT_RECONCILIATION_SCHEMA_VERSION: u16 = 1;

/// Runtime-safe outcome of a read-only reconciliation probe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectReconciliationOutcomeV1 {
    ObservedApplied,
    ObservedNotApplied,
    StillUncertain,
}

/// Read-only observation class admitted for an uncertain effect.  The kind is part of the
/// durable request so a recovery worker cannot substitute an arbitrary shell command merely to
/// obtain a convenient answer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ReconciliationProbeKindV1 {
    WorkspaceObservation,
    ProviderReceipt,
    ExternalReceipt,
}

impl ReconciliationProbeKindV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceObservation => "workspace_observation",
            Self::ProviderReceipt => "provider_receipt",
            Self::ExternalReceipt => "external_receipt",
        }
    }
}

/// Exact, bounded request handed to a registered read-only reconciliation probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectReconciliationProbeRequestV1 {
    pub schema_version: u16,
    pub reconciliation_id: String,
    pub effect_id: String,
    pub effect_digest: String,
    pub replay_contract_fingerprint: String,
    pub probe_id: String,
    pub probe_kind: ReconciliationProbeKindV1,
    pub budget_ms: u64,
}

impl EffectReconciliationProbeRequestV1 {
    /// Validates the exact bounded authority supplied to a read-only probe.
    pub fn validate(&self) -> Result<()> {
        validate_probe_request(self)
    }
}

/// Durable single-owner claim for one reconciliation probe.  A claim never authorizes a replay
/// of the original effect; it only authorizes the declared read-only observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectReconciliationProbeStartedEntryV1 {
    pub schema_version: u16,
    pub reconciliation_id: String,
    pub effect_id: String,
    pub effect_digest: String,
    pub probe_id: String,
    pub probe_kind: ReconciliationProbeKindV1,
    pub claimed_at_unix_ms: u64,
}

/// Result returned by a registered read-only reconciliation probe before the runtime attempts
/// the conditional terminal append.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectReconciliationProbeReceiptV1 {
    pub schema_version: u16,
    pub reconciliation_id: String,
    pub effect_id: String,
    pub effect_digest: String,
    pub probe_id: String,
    pub outcome: EffectReconciliationOutcomeV1,
    pub receipt_digest: String,
    pub observed_at_unix_ms: u64,
}

impl EffectReconciliationProbeReceiptV1 {
    /// Validates a probe receipt before it can be bound to a durable terminal settlement.
    pub fn validate(&self) -> Result<()> {
        validate_probe_receipt(self)
    }
}

/// Runtime-owned read-only observation extension point.  Implementations must reject requests
/// outside their declared kind and must not invoke the original effect as part of observation.
#[async_trait]
pub trait EffectReconciliationProbe: Send + Sync {
    fn kind(&self) -> ReconciliationProbeKindV1;

    fn validate(&self, request: &EffectReconciliationProbeRequestV1) -> Result<()>;

    async fn observe(
        &self,
        request: EffectReconciliationProbeRequestV1,
    ) -> Result<EffectReconciliationProbeReceiptV1>;
}

/// Durable authority requiring an exact effect to be observed before any successor is admitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectReconciliationRequiredEntryV1 {
    pub schema_version: u16,
    pub reconciliation_id: String,
    pub effect_id: String,
    pub effect_digest: String,
    pub replay_contract_fingerprint: String,
    pub reason_code: String,
    pub requested_at_unix_ms: u64,
    /// Optional V1 sidecar facts.  Historic direct events omit these fields and remain readable;
    /// new writers supply them when the owning tool boundary knows the exact relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_workspace_observation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_workspace_observation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_receipt_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_probe_kinds: Vec<ReconciliationProbeKindV1>,
    #[serde(default)]
    pub probe_budget_ms: u64,
}

/// Durable closure for one exact reconciliation request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EffectReconciliationTerminalEntryV1 {
    pub schema_version: u16,
    pub reconciliation_id: String,
    pub effect_id: String,
    pub effect_digest: String,
    pub outcome: EffectReconciliationOutcomeV1,
    pub probe_receipt_digest: Option<String>,
    /// Present for all new probe-owned terminal records.  Missing is accepted only for historic
    /// V1 terminal records written before the probe-claim lifecycle existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_id: Option<String>,
}

/// Read-model preventing an uncertain effect from receiving a duplicate terminal or replay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectReconciliationProjectionV1 {
    cursor: Option<ProjectionCursor>,
    required: BTreeMap<String, EffectReconciliationRequiredEntryV1>,
    probes: BTreeMap<String, EffectReconciliationProbeStartedEntryV1>,
    terminal: BTreeMap<String, EffectReconciliationTerminalEntryV1>,
}

impl EffectReconciliationProjectionV1 {
    /// Rebuilds reconciliation state from the critical durable stream, rejecting sequence gaps
    /// and malformed newer critical events instead of silently allowing a blind replay.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if projection_apply_decision(self.cursor.as_ref(), event)?
            == ProjectionApplyDecision::IgnoreAlreadyApplied
        {
            return Ok(());
        }
        match decode_stored_event(event.clone())? {
            StoredEventDecode::Known(_) | StoredEventDecode::UnknownNonCritical(_) => {}
        }
        match event.event_kind() {
            Some(DurableEventType::EffectReconciliationRequired) => {
                self.apply_required(decode(event)?)?;
            }
            Some(DurableEventType::EffectReconciliationProbeStarted) => {
                self.apply_probe_started(decode(event)?)?;
            }
            Some(DurableEventType::EffectReconciliationTerminal) => {
                self.apply_terminal(decode(event)?)?;
            }
            Some(_) | None => {}
        }
        self.cursor = Some(record.projection_cursor(EFFECT_RECONCILIATION_SCHEMA_VERSION));
        Ok(())
    }

    pub fn apply_required(&mut self, entry: EffectReconciliationRequiredEntryV1) -> Result<()> {
        validate_required(&entry)?;
        if self
            .required
            .insert(entry.reconciliation_id.clone(), entry)
            .is_some()
        {
            bail!("effect reconciliation was required more than once");
        }
        Ok(())
    }

    pub fn apply_probe_started(
        &mut self,
        entry: EffectReconciliationProbeStartedEntryV1,
    ) -> Result<()> {
        validate_probe_started(&entry)?;
        let required = self.required.get(&entry.reconciliation_id).ok_or_else(|| {
            anyhow::anyhow!("effect reconciliation probe has no required predecessor")
        })?;
        if required.effect_id != entry.effect_id || required.effect_digest != entry.effect_digest {
            bail!("effect reconciliation probe does not match its required effect");
        }
        if !required.allowed_probe_kinds.is_empty()
            && !required.allowed_probe_kinds.contains(&entry.probe_kind)
        {
            bail!("effect reconciliation probe kind is not allowed by its requirement");
        }
        if self.terminal.contains_key(&entry.reconciliation_id) {
            bail!("effect reconciliation probe cannot start after a terminal settlement");
        }
        if self
            .probes
            .insert(entry.reconciliation_id.clone(), entry)
            .is_some()
        {
            bail!("effect reconciliation already has an active probe claim");
        }
        Ok(())
    }

    pub fn apply_terminal(&mut self, entry: EffectReconciliationTerminalEntryV1) -> Result<()> {
        validate_terminal(&entry)?;
        let required = self.required.get(&entry.reconciliation_id).ok_or_else(|| {
            anyhow::anyhow!("effect reconciliation terminal has no required predecessor")
        })?;
        if required.effect_id != entry.effect_id || required.effect_digest != entry.effect_digest {
            bail!("effect reconciliation terminal does not match its required effect");
        }
        match (&entry.probe_id, self.probes.get(&entry.reconciliation_id)) {
            (Some(probe_id), Some(probe)) if probe.probe_id == *probe_id => {
                if entry.probe_receipt_digest.is_none() {
                    bail!("probe-owned reconciliation terminal is missing its receipt digest");
                }
            }
            (Some(_), Some(_)) => {
                bail!("effect reconciliation terminal probe does not match claim")
            }
            (Some(_), None) => bail!("effect reconciliation terminal has no matching probe claim"),
            (None, Some(_)) => bail!("new reconciliation probe claim requires a bound terminal"),
            // Historic V1 direct records did not have a probe claim.  Preserve read compatibility
            // without allowing new writers to bypass the claim once one exists.
            (None, None) => {}
        }
        if self
            .terminal
            .insert(entry.reconciliation_id.clone(), entry)
            .is_some()
        {
            bail!("effect reconciliation was terminal more than once");
        }
        Ok(())
    }

    #[must_use]
    pub fn terminal(
        &self,
        reconciliation_id: &str,
    ) -> Option<&EffectReconciliationTerminalEntryV1> {
        self.terminal.get(reconciliation_id)
    }

    #[must_use]
    pub fn required(
        &self,
        reconciliation_id: &str,
    ) -> Option<&EffectReconciliationRequiredEntryV1> {
        self.required.get(reconciliation_id)
    }

    #[must_use]
    pub fn active_probe(
        &self,
        reconciliation_id: &str,
    ) -> Option<&EffectReconciliationProbeStartedEntryV1> {
        (!self.terminal.contains_key(reconciliation_id))
            .then(|| self.probes.get(reconciliation_id))
            .flatten()
    }

    /// Lists exact effects still blocked from replay.
    #[must_use]
    pub fn active(&self) -> Vec<&EffectReconciliationRequiredEntryV1> {
        self.required
            .iter()
            .filter(|(id, _)| !self.terminal.contains_key(*id))
            .map(|(_, entry)| entry)
            .collect()
    }

    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }
}

pub(crate) fn validate_required(entry: &EffectReconciliationRequiredEntryV1) -> Result<()> {
    if entry.schema_version != EFFECT_RECONCILIATION_SCHEMA_VERSION
        || entry.reconciliation_id.trim().is_empty()
        || entry.effect_id.trim().is_empty()
        || !entry.effect_digest.starts_with("sha256:")
        || entry.replay_contract_fingerprint.trim().is_empty()
        || !safe_code(&entry.reason_code)
    {
        bail!("effect reconciliation required entry is malformed");
    }
    if entry.known_receipt_ids.len() > 32 || entry.allowed_probe_kinds.len() > 3 {
        bail!("effect reconciliation required entry exceeds bounded sidecar fields");
    }
    if entry.probe_budget_ms > 60_000 {
        bail!("effect reconciliation probe budget exceeds the bounded maximum");
    }
    for value in entry
        .logical_run_id
        .iter()
        .chain(entry.task_id.iter())
        .chain(entry.step_id.iter())
        .chain(entry.participant_attempt_id.iter())
        .chain(entry.base_workspace_observation_id.iter())
        .chain(entry.current_workspace_observation_id.iter())
        .chain(entry.known_receipt_ids.iter())
    {
        if value.is_empty() || value.len() > 512 || crate::safe_persistence_text(value) != *value {
            bail!("effect reconciliation sidecar field is malformed");
        }
    }
    Ok(())
}

pub(crate) fn validate_probe_request(entry: &EffectReconciliationProbeRequestV1) -> Result<()> {
    if entry.schema_version != EFFECT_RECONCILIATION_SCHEMA_VERSION
        || entry.reconciliation_id.trim().is_empty()
        || entry.effect_id.trim().is_empty()
        || !entry.effect_digest.starts_with("sha256:")
        || entry.replay_contract_fingerprint.trim().is_empty()
        || entry.probe_id.trim().is_empty()
        || entry.budget_ms == 0
        || entry.budget_ms > 60_000
    {
        bail!("effect reconciliation probe request is malformed");
    }
    Ok(())
}

pub(crate) fn validate_probe_started(
    entry: &EffectReconciliationProbeStartedEntryV1,
) -> Result<()> {
    if entry.schema_version != EFFECT_RECONCILIATION_SCHEMA_VERSION
        || entry.reconciliation_id.trim().is_empty()
        || entry.effect_id.trim().is_empty()
        || !entry.effect_digest.starts_with("sha256:")
        || entry.probe_id.trim().is_empty()
        || entry.claimed_at_unix_ms == 0
    {
        bail!("effect reconciliation probe start entry is malformed");
    }
    Ok(())
}

pub(crate) fn validate_probe_receipt(entry: &EffectReconciliationProbeReceiptV1) -> Result<()> {
    if entry.schema_version != EFFECT_RECONCILIATION_SCHEMA_VERSION
        || entry.reconciliation_id.trim().is_empty()
        || entry.effect_id.trim().is_empty()
        || !entry.effect_digest.starts_with("sha256:")
        || entry.probe_id.trim().is_empty()
        || !entry.receipt_digest.starts_with("sha256:")
        || entry.observed_at_unix_ms == 0
    {
        bail!("effect reconciliation probe receipt is malformed");
    }
    Ok(())
}

pub(crate) fn validate_terminal(entry: &EffectReconciliationTerminalEntryV1) -> Result<()> {
    if entry.schema_version != EFFECT_RECONCILIATION_SCHEMA_VERSION
        || entry.reconciliation_id.trim().is_empty()
        || entry.effect_id.trim().is_empty()
        || !entry.effect_digest.starts_with("sha256:")
        || entry
            .probe_receipt_digest
            .as_deref()
            .is_some_and(|digest| !digest.starts_with("sha256:"))
        || entry
            .probe_id
            .as_deref()
            .is_some_and(|probe_id| probe_id.trim().is_empty())
    {
        bail!("effect reconciliation terminal entry is malformed");
    }
    Ok(())
}

fn safe_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn decode<T: serde::de::DeserializeOwned>(event: &StoredEvent) -> Result<T> {
    serde_json::from_value(event.payload.clone())
        .context("failed to decode effect reconciliation event")
}

#[cfg(test)]
#[path = "tests/effect_reconciliation_tests.rs"]
mod tests;
