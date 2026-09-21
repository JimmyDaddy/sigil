//! Durable capability selection for one session. This contains no module configuration,
//! credentials, host paths, or concrete runtime implementations.

use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::config::OptionalCapability;

pub const SESSION_COMPOSITION_SCHEMA_VERSION: u16 = 1;
pub const CORE_RUN_CONTRACT_VERSION: u16 = 1;

/// Immutable contracts selected before this session's first effectful run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCompositionSnapshotV1 {
    pub schema_version: u16,
    pub core_contract_version: u16,
    pub capabilities: BTreeSet<OptionalCapability>,
}

impl SessionCompositionSnapshotV1 {
    #[must_use]
    pub fn new(capabilities: BTreeSet<OptionalCapability>) -> Self {
        Self {
            schema_version: SESSION_COMPOSITION_SCHEMA_VERSION,
            core_contract_version: CORE_RUN_CONTRACT_VERSION,
            capabilities,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SESSION_COMPOSITION_SCHEMA_VERSION,
            "unsupported session composition schema; start a new session"
        );
        ensure!(
            self.core_contract_version == CORE_RUN_CONTRACT_VERSION,
            "unsupported core execution contract; start a new session"
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/session_composition_tests.rs"]
mod tests;
