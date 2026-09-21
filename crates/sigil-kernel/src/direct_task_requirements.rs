//! Immutable Direct requirement records retained for reading existing session history.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::TaskId;

/// An interpretation tied to an exact UTF-8 byte span of the original durable objective.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectTaskRequirementV1 {
    pub start_byte: u32,
    pub end_byte: u32,
    pub interpretation: String,
    pub required: bool,
}

/// Historical admission baseline. New runs do not write or consume this protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskDirectRequirementsBoundV1 {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub admission_id: String,
    pub objective_hash: String,
    pub requirements: Vec<DirectTaskRequirementV1>,
    pub baseline_digest: String,
}

impl TaskDirectRequirementsBoundV1 {
    fn digest(&self) -> Result<String> {
        Ok(format!(
            "sha256:{}",
            crate::sha256_hex(&serde_json::to_vec(&(
                self.schema_version,
                &self.task_id,
                &self.admission_id,
                &self.objective_hash,
                &self.requirements,
            ))?)
        ))
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.admission_id.is_empty()
            || self.admission_id.len() > 128
            || self.objective_hash.len() != 71
            || !self.objective_hash.starts_with("sha256:")
            || !(1..=128).contains(&self.requirements.len())
            || self.baseline_digest != self.digest()?
        {
            bail!("invalid direct requirement baseline identity or bounds");
        }
        for item in &self.requirements {
            if item.start_byte >= item.end_byte
                || item.interpretation.trim().is_empty()
                || item.interpretation.len() > 4096
                || crate::safe_persistence_text(&item.interpretation) != item.interpretation
            {
                bail!("invalid direct requirement interpretation or source span");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/direct_task_requirements_tests.rs"]
mod tests;
