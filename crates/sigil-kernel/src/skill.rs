use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    session::{ControlEntry, SessionLogEntry},
    tool::ToolRegistryScope,
};

/// Provider-neutral descriptor for one reusable skill workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SkillDescriptor {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub root: PathBuf,
    #[serde(default)]
    pub entrypoint: PathBuf,
    #[serde(default)]
    pub source: SkillSource,
    #[serde(default)]
    pub sha256: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub trust: SkillTrustState,
    #[serde(default = "default_true")]
    pub model_invocable: bool,
    #[serde(default = "default_true")]
    pub user_invocable: bool,
    #[serde(default)]
    pub run_as: SkillRunMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
    #[serde(default)]
    pub allowed_tools: ToolRegistryScope,
    #[serde(default)]
    pub disallowed_tools: ToolRegistryScope,
    #[serde(default)]
    pub path_patterns: Vec<String>,
}

/// Where a skill descriptor was discovered.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SkillSource {
    #[default]
    Workspace,
    User,
    Plugin {
        plugin_id: String,
    },
}

impl SkillSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::User => "user",
            Self::Plugin { .. } => "plugin",
        }
    }

    fn sort_key(&self) -> String {
        match self {
            Self::Workspace => "0:workspace".to_owned(),
            Self::User => "1:user".to_owned(),
            Self::Plugin { plugin_id } => format!("2:plugin:{plugin_id}"),
        }
    }
}

/// Trust state applied before a skill can be loaded or invoked.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillTrustState {
    Trusted,
    #[default]
    NeedsReview,
    Disabled,
}

impl SkillTrustState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::NeedsReview => "needs_review",
            Self::Disabled => "disabled",
        }
    }
}

/// How Sigil should execute a user-invoked skill.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillRunMode {
    #[default]
    Inline,
    ChildSession,
}

impl SkillRunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::ChildSession => "child_session",
        }
    }
}

/// Durable snapshot of the discovered skill index.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SkillIndexSnapshot {
    #[serde(default)]
    pub descriptors: Vec<SkillDescriptor>,
    #[serde(default)]
    pub fingerprint: String,
}

impl<'de> Deserialize<'de> for SkillIndexSnapshot {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        struct Wire {
            #[serde(default)]
            descriptors: Vec<SkillDescriptor>,
            #[serde(default)]
            fingerprint: String,
        }

        let Wire {
            descriptors,
            fingerprint: _legacy_fingerprint,
        } = Wire::deserialize(deserializer)?;
        let mut descriptors = descriptors;
        sort_skill_descriptors(&mut descriptors);
        let fingerprint =
            skill_index_fingerprint(&descriptors).map_err(serde::de::Error::custom)?;
        Ok(Self {
            descriptors,
            fingerprint,
        })
    }
}

impl SkillIndexSnapshot {
    /// Builds a snapshot with descriptors sorted into a deterministic replay order.
    ///
    /// # Errors
    ///
    /// Returns an error if the descriptor list cannot be serialized for hashing.
    pub fn new(mut descriptors: Vec<SkillDescriptor>) -> Result<Self> {
        sort_skill_descriptors(&mut descriptors);
        let fingerprint = skill_index_fingerprint(&descriptors)?;
        Ok(Self {
            descriptors,
            fingerprint,
        })
    }

    /// Recomputes and replaces the snapshot fingerprint after descriptor changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the descriptor list cannot be serialized for hashing.
    pub fn refresh_fingerprint(&mut self) -> Result<()> {
        sort_skill_descriptors(&mut self.descriptors);
        self.fingerprint = skill_index_fingerprint(&self.descriptors)?;
        Ok(())
    }
}

/// Append-only record for a loaded skill body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SkillLoadEntry {
    pub skill_id: String,
    pub sha256: String,
    pub source: SkillSource,
    pub entrypoint: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub byte_count: u64,
    pub line_count: u64,
    pub loaded_at_ms: u64,
}

/// Latest skill state reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillStateProjection {
    pub latest_index: Option<SkillIndexSnapshot>,
    pub loaded_skills: BTreeMap<String, SkillLoadState>,
    pub latest_loaded_skill_id: Option<String>,
    pub load_replay_order: Vec<String>,
}

impl SkillStateProjection {
    /// Replays append-only session entries into the latest skill projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(control);
            }
        }
        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::SkillIndexCaptured(snapshot) => {
                self.latest_index = Some(snapshot.clone());
            }
            ControlEntry::SkillLoaded(entry) => self.apply_loaded(entry),
            _ => {}
        }
    }

    pub fn latest_loaded(&self) -> Option<&SkillLoadState> {
        self.latest_loaded_skill_id
            .as_ref()
            .and_then(|id| self.loaded_skills.get(id))
    }

    fn apply_loaded(&mut self, entry: &SkillLoadEntry) {
        self.latest_loaded_skill_id = Some(entry.skill_id.clone());
        self.load_replay_order.push(entry.skill_id.clone());
        self.loaded_skills.insert(
            entry.skill_id.clone(),
            SkillLoadState {
                entry: entry.clone(),
            },
        );
    }
}

/// Latest projected loaded state for one skill id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillLoadState {
    pub entry: SkillLoadEntry,
}

fn sort_skill_descriptors(descriptors: &mut [SkillDescriptor]) {
    descriptors.sort_by(|left, right| {
        (
            left.id.as_str(),
            left.source.sort_key(),
            left.entrypoint.as_os_str(),
        )
            .cmp(&(
                right.id.as_str(),
                right.source.sort_key(),
                right.entrypoint.as_os_str(),
            ))
    });
}

fn skill_index_fingerprint(descriptors: &[SkillDescriptor]) -> Result<String> {
    if descriptors.is_empty() {
        return Ok("none".to_owned());
    }

    let payload = serde_json::to_vec(
        &descriptors
            .iter()
            .map(SkillIndexFingerprintDescriptor::from)
            .collect::<Vec<_>>(),
    )?;
    Ok(format!("{:x}", Sha256::digest(payload)))
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct SkillIndexFingerprintDescriptor<'a> {
    id: &'a str,
    name: &'a str,
    description: &'a str,
    when_to_use: Option<&'a str>,
    source: &'a SkillSource,
    sha256: &'a str,
    enabled: bool,
    trust: SkillTrustState,
    model_invocable: bool,
    user_invocable: bool,
    run_as: SkillRunMode,
    argument_hint: Option<&'a str>,
}

impl<'a> From<&'a SkillDescriptor> for SkillIndexFingerprintDescriptor<'a> {
    fn from(descriptor: &'a SkillDescriptor) -> Self {
        Self {
            id: &descriptor.id,
            name: &descriptor.name,
            description: &descriptor.description,
            when_to_use: descriptor.when_to_use.as_deref(),
            source: &descriptor.source,
            sha256: &descriptor.sha256,
            enabled: descriptor.enabled,
            trust: descriptor.trust,
            model_invocable: descriptor.model_invocable,
            user_invocable: descriptor.user_invocable,
            run_as: descriptor.run_as,
            argument_hint: descriptor.argument_hint.as_deref(),
        }
    }
}

#[cfg(test)]
#[path = "tests/skill_tests.rs"]
mod tests;
