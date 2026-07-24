use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{ControlEntry, SessionLogEntry};

/// Zero-tolerance orchestration invariant that disables one exact route and build.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrchestrationHardInvariant {
    DuplicateHandoff,
    DuplicateSpawn,
    DuplicateContinuation,
    DuplicateMerge,
    PermissionMonotonicityViolation,
    UnknownEffectReplay,
    ParentChildDuplicateFinal,
    ModelPollingTurn,
}

/// Append-only local kill-switch fact for one exact provider route and Sigil build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct OrchestrationRouteDisabledEntry {
    pub route_fingerprint: String,
    pub sigil_build: String,
    pub invariant: OrchestrationHardInvariant,
    pub report_handle: String,
    pub disabled_at_ms: u64,
}

impl OrchestrationRouteDisabledEntry {
    /// Validates the durable route-disable payload before it is admitted or replayed.
    ///
    /// # Errors
    ///
    /// Returns an error when the route, build, report handle, or timestamp is unsafe or missing.
    pub fn validate(&self) -> Result<()> {
        if self.route_fingerprint.len() != 71
            || !self.route_fingerprint.starts_with("sha256:")
            || !self.route_fingerprint[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("orchestration route disablement has an invalid route fingerprint");
        }
        validate_safe_field("Sigil build", &self.sigil_build, 256)?;
        validate_safe_field("report handle", &self.report_handle, 512)?;
        if self.disabled_at_ms == 0 {
            bail!("orchestration route disablement timestamp is zero");
        }
        Ok(())
    }
}

/// Current route-local disablements reconstructed from append-only session facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrchestrationRouteDisablementProjection {
    disabled: BTreeMap<(String, String), OrchestrationRouteDisabledEntry>,
}

impl OrchestrationRouteDisablementProjection {
    #[must_use]
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(disabled)) =
                entry
            else {
                continue;
            };
            projection
                .disabled
                .entry((
                    disabled.route_fingerprint.clone(),
                    disabled.sigil_build.clone(),
                ))
                .or_insert_with(|| disabled.clone());
        }
        projection
    }

    #[must_use]
    pub fn disablement_for(
        &self,
        route_fingerprint: &str,
        sigil_build: &str,
    ) -> Option<&OrchestrationRouteDisabledEntry> {
        self.disabled
            .get(&(route_fingerprint.to_owned(), sigil_build.to_owned()))
    }

    #[must_use]
    pub fn is_disabled(&self, route_fingerprint: &str, sigil_build: &str) -> bool {
        self.disablement_for(route_fingerprint, sigil_build)
            .is_some()
    }
}

fn validate_safe_field(label: &str, value: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        bail!("orchestration route disablement {label} is invalid");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/orchestration_tests.rs"]
mod tests;
