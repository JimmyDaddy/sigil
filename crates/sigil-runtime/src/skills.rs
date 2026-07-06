use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use sigil_kernel::{PluginSkillRef, SkillDescriptor, SkillIndexSnapshot};

pub const LOAD_SKILL_TOOL_NAME: &str = "load_skill";

mod discovery;
mod frontmatter;
mod load;
use discovery::{SkillCandidateKind, valid_skill_id};
pub use discovery::{discover_skill_index, discover_skill_index_with_user_dir};
#[cfg(test)]
use frontmatter::{clean_scalar, parse_inline_list};
use frontmatter::{descriptor_from_entrypoint, fallback_skill_id};
#[cfg(test)]
use load::{LoadSkillTool, model_visible_skill_index_description, resolved_descriptor_path};
pub use load::{LoadedSkillContext, load_user_invoked_skill, register_skill_tools};

/// Result of skill discovery, including the deterministic index and non-fatal warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryReport {
    pub snapshot: SkillIndexSnapshot,
    pub warnings: Vec<SkillDiscoveryWarning>,
}

/// One non-fatal problem found while discovering skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiscoveryWarning {
    pub kind: SkillDiscoveryWarningKind,
    pub path: PathBuf,
    pub message: String,
}

/// Stable warning categories for discovery diagnostics and future TUI display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiscoveryWarningKind {
    InvalidPath,
    InvalidName,
    InvalidFrontmatter,
    ReadFailed,
    Shadowed,
}

/// Builds the stable runtime id for a plugin-provided skill.
///
/// # Errors
///
/// Returns an error when either segment is not a valid skill id segment.
pub fn namespaced_plugin_skill_id(plugin_id: &str, skill_id: &str) -> Result<String> {
    if !valid_skill_id(plugin_id) {
        bail!("invalid plugin id {plugin_id:?}");
    }
    if !valid_skill_id(skill_id) {
        bail!("invalid plugin skill id {skill_id:?}");
    }
    Ok(format!("{plugin_id}/{skill_id}"))
}

/// Discovers plugin-owned skills from manifest-relative entries under one plugin root.
///
/// # Errors
///
/// Returns an error when a referenced skill path is unsafe, missing, unreadable, malformed, or
/// duplicated within the plugin manifest.
pub fn discover_plugin_skill_descriptors(
    workspace_root: &Path,
    plugin_id: &str,
    plugin_root: &Path,
    skills: &[PluginSkillRef],
) -> Result<Vec<SkillDescriptor>> {
    let canonical_workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let canonical_plugin_root = plugin_root
        .canonicalize()
        .with_context(|| format!("failed to resolve plugin root {}", plugin_root.display()))?;
    if !canonical_plugin_root.starts_with(&canonical_workspace_root) {
        bail!("plugin {plugin_id} root escapes workspace root");
    }

    let mut descriptors = Vec::new();
    let mut claimed_ids = BTreeSet::new();
    for skill in skills {
        let entrypoint = plugin_root.join(&skill.path);
        let canonical_entrypoint = entrypoint.canonicalize().with_context(|| {
            format!(
                "failed to resolve plugin {plugin_id} skill {}",
                skill.path.display()
            )
        })?;
        if !canonical_entrypoint.starts_with(&canonical_plugin_root) {
            bail!(
                "plugin {plugin_id} skill path escapes plugin root: {}",
                skill.path.display()
            );
        }
        let logical_entrypoint = plugin_root.join(&skill.path);
        let fallback_id = fallback_skill_id(&skill.path)?;
        let descriptor = descriptor_from_entrypoint(
            workspace_root,
            logical_entrypoint.parent().unwrap_or(plugin_root),
            &logical_entrypoint,
            &fallback_id,
            &SkillCandidateKind::PluginSkill {
                plugin_id: plugin_id.to_owned(),
            },
        )?;
        if !claimed_ids.insert(descriptor.id.clone()) {
            bail!(
                "plugin {plugin_id} declares duplicate skill {}",
                descriptor.id
            );
        }
        descriptors.push(descriptor);
    }
    Ok(descriptors)
}

#[cfg(test)]
#[path = "tests/skills_tests.rs"]
mod tests;
