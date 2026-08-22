use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::*;
use crate::{
    RecoveryBlockerRaisedV1, RecoveryBlockerResolutionStartedV1, RecoveryBlockerResolvedV1,
    RecoveryBlockerSupersededV1, RecoveryBlockerV1, projection_apply_decision,
};

/// Current schema for the replay-only recovery blocker projector.
pub const RECOVERY_BLOCKER_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// State retained for one active resolution claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryBlockerResolutionStateV1 {
    pub action: crate::RecoveryActionV1,
    pub attempt_id: String,
    pub started_at_ms: u64,
}

/// Strict recovery lifecycle projection rebuilt from direct durable events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryBlockerProjectionV1 {
    cursor: Option<ProjectionCursor>,
    active: BTreeMap<String, RecoveryBlockerV1>,
    resolved: BTreeMap<String, RecoveryBlockerResolvedV1>,
    superseded: BTreeMap<String, RecoveryBlockerSupersededV1>,
    resolution_started: BTreeMap<String, RecoveryBlockerResolutionStateV1>,
    evidence_to_blocker: BTreeMap<String, String>,
}

impl RecoveryBlockerProjectionV1 {
    /// Rebuilds recovery state while rejecting duplicate, mismatched or out-of-order terminals.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    #[must_use]
    pub fn active(&self) -> Vec<&RecoveryBlockerV1> {
        self.active.values().collect()
    }

    #[must_use]
    pub fn active_blocker(&self, blocker_id: &str) -> Option<&RecoveryBlockerV1> {
        self.active.get(blocker_id)
    }

    #[must_use]
    pub fn resolved(&self, blocker_id: &str) -> Option<&RecoveryBlockerResolvedV1> {
        self.resolved.get(blocker_id)
    }

    /// Returns the durable resolution claim, if a recovery action already owns this blocker.
    #[must_use]
    pub fn resolution_started(
        &self,
        blocker_id: &str,
    ) -> Option<&RecoveryBlockerResolutionStateV1> {
        self.resolution_started.get(blocker_id)
    }

    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
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
            Some(DurableEventType::RecoveryBlockerRaised) => {
                self.apply_raised(decode(event)?)?;
            }
            Some(DurableEventType::RecoveryBlockerResolutionStarted) => {
                self.apply_resolution_started(decode(event)?)?;
            }
            Some(DurableEventType::RecoveryBlockerResolved) => {
                self.apply_resolved(decode(event)?)?;
            }
            Some(DurableEventType::RecoveryBlockerSuperseded) => {
                self.apply_superseded(decode(event)?)?;
            }
            Some(_) | None => {}
        }
        self.cursor = Some(record.projection_cursor(RECOVERY_BLOCKER_PROJECTION_SCHEMA_VERSION));
        Ok(())
    }

    fn apply_raised(&mut self, entry: RecoveryBlockerRaisedV1) -> Result<()> {
        entry.validate()?;
        let blocker = entry.blocker;
        if let Some(existing) = self.evidence_to_blocker.get(&blocker.evidence_digest) {
            if existing == &blocker.blocker_id {
                bail!("recovery blocker evidence was raised more than once");
            }
            bail!("recovery blocker evidence is already bound to another blocker");
        }
        if self.active.contains_key(&blocker.blocker_id)
            || self.resolved.contains_key(&blocker.blocker_id)
            || self.superseded.contains_key(&blocker.blocker_id)
        {
            bail!("recovery blocker lifecycle reuses a blocker id");
        }
        self.evidence_to_blocker
            .insert(blocker.evidence_digest.clone(), blocker.blocker_id.clone());
        self.active.insert(blocker.blocker_id.clone(), blocker);
        Ok(())
    }

    fn apply_resolution_started(
        &mut self,
        entry: RecoveryBlockerResolutionStartedV1,
    ) -> Result<()> {
        entry.validate()?;
        let blocker = self
            .active
            .get(&entry.blocker_id)
            .context("recovery resolution start has no active blocker")?;
        if !blocker.available_actions.contains(&entry.action) {
            bail!("recovery resolution action is not allowed by its blocker");
        }
        if self.resolution_started.contains_key(&entry.blocker_id) {
            bail!("recovery blocker resolution was started more than once");
        }
        self.resolution_started.insert(
            entry.blocker_id,
            RecoveryBlockerResolutionStateV1 {
                action: entry.action,
                attempt_id: entry.attempt_id,
                started_at_ms: entry.started_at_ms,
            },
        );
        Ok(())
    }

    fn apply_resolved(&mut self, entry: RecoveryBlockerResolvedV1) -> Result<()> {
        entry.validate()?;
        let blocker = self
            .active
            .get(&entry.blocker_id)
            .context("recovery resolution has no active blocker")?;
        if entry.resolved_at_ms < blocker.created_at_ms {
            bail!("recovery blocker resolved before it was raised");
        }
        if !self.resolution_started.contains_key(&entry.blocker_id) {
            bail!("recovery blocker resolution has no durable resolution start");
        }
        if self.resolved.contains_key(&entry.blocker_id) {
            bail!("recovery blocker was resolved more than once");
        }
        self.active.remove(&entry.blocker_id);
        self.resolution_started.remove(&entry.blocker_id);
        self.resolved.insert(entry.blocker_id.clone(), entry);
        Ok(())
    }

    fn apply_superseded(&mut self, entry: RecoveryBlockerSupersededV1) -> Result<()> {
        entry.validate()?;
        if !self.active.contains_key(&entry.blocker_id) {
            bail!("recovery supersession has no active predecessor blocker");
        }
        if !self.active.contains_key(&entry.successor_blocker_id) {
            bail!("recovery supersession has no active successor blocker");
        }
        if self.superseded.contains_key(&entry.blocker_id) {
            bail!("recovery blocker was superseded more than once");
        }
        self.active.remove(&entry.blocker_id);
        self.resolution_started.remove(&entry.blocker_id);
        self.superseded.insert(entry.blocker_id.clone(), entry);
        Ok(())
    }
}

fn decode<T: serde::de::DeserializeOwned>(event: &StoredEvent) -> Result<T> {
    serde_json::from_value(event.payload.clone()).context("failed to decode recovery blocker event")
}
