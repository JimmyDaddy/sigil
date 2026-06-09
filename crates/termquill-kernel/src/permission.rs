use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, anyhow};
use globset::Glob;
use serde::{Deserialize, Serialize};

use crate::tool::{ToolAccess, ToolSpec, ToolSubject, ToolSubjectScope};

/// Default interaction surface for one agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Interactive,
    Headless,
}

/// Stable approval modes used by permission policy evaluation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Allow,
    #[default]
    Ask,
    Deny,
}

impl ApprovalMode {
    /// Returns the stable config-friendly label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// Per-access permission defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionAccessConfig {
    #[serde(default)]
    pub read: Option<ApprovalMode>,
    #[serde(default)]
    pub write: Option<ApprovalMode>,
    #[serde(default)]
    pub execute: Option<ApprovalMode>,
    #[serde(default)]
    pub network: Option<ApprovalMode>,
}

impl Default for PermissionAccessConfig {
    fn default() -> Self {
        Self {
            read: Some(ApprovalMode::Allow),
            write: None,
            execute: None,
            network: None,
        }
    }
}

impl PermissionAccessConfig {
    fn mode_for(&self, access: ToolAccess) -> Option<ApprovalMode> {
        match access {
            ToolAccess::Read => self.read,
            ToolAccess::Write => self.write,
            ToolAccess::Execute => self.execute,
            ToolAccess::Network => self.network,
        }
    }
}

/// One explicit tool permission override rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionRule {
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub subject_glob: Option<String>,
    #[serde(default)]
    pub mode: ApprovalMode,
}

/// Advanced guard for explicitly approved paths outside the workspace root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExternalDirectoryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub default_mode: ApprovalMode,
    #[serde(default)]
    pub rules: Vec<ExternalDirectoryRule>,
}

impl Default for ExternalDirectoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_mode: ApprovalMode::Ask,
            rules: Vec::new(),
        }
    }
}

/// One external-directory permission override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExternalDirectoryRule {
    pub path_glob: String,
    #[serde(default)]
    pub mode: ApprovalMode,
}

/// Shared permission policy configuration for one entrypoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionConfig {
    #[serde(default)]
    pub default_mode: ApprovalMode,
    #[serde(default)]
    pub access: PermissionAccessConfig,
    #[serde(default)]
    pub tools: BTreeMap<String, ApprovalMode>,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    #[serde(default)]
    pub external_directory: ExternalDirectoryConfig,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            default_mode: ApprovalMode::Ask,
            access: PermissionAccessConfig::default(),
            tools: BTreeMap::new(),
            rules: Vec::new(),
            external_directory: ExternalDirectoryConfig::default(),
        }
    }
}

/// One resolved permission decision for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub mode: ApprovalMode,
    pub access: ToolAccess,
    pub subjects: Vec<ToolSubject>,
    pub external_directory_required: bool,
}

/// Policy evaluator that resolves allow/ask/deny for one tool call.
pub struct PermissionPolicy<'a> {
    config: &'a PermissionConfig,
}

impl<'a> PermissionPolicy<'a> {
    /// Creates a policy evaluator from shared configuration.
    pub fn new(config: &'a PermissionConfig) -> Self {
        Self { config }
    }

    /// Resolves one tool call decision from the tool spec, stable name, and subjects.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide(
        &self,
        spec: &ToolSpec,
        tool_name: &str,
        subjects: Vec<ToolSubject>,
    ) -> Result<PermissionDecision> {
        self.decide_with_access(spec, tool_name, spec.access, subjects)
    }

    /// Resolves one tool call decision using a dynamic access class derived from call arguments.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide_with_access(
        &self,
        _spec: &ToolSpec,
        tool_name: &str,
        access: ToolAccess,
        subjects: Vec<ToolSubject>,
    ) -> Result<PermissionDecision> {
        let external_directory_required = subjects
            .iter()
            .any(|subject| subject.scope == ToolSubjectScope::External)
            && !self.config.external_directory.enabled;
        let subject_modes = if subjects.is_empty() {
            vec![self.decide_one_subject(tool_name, access, None)?]
        } else {
            subjects
                .iter()
                .map(|subject| self.decide_one_subject(tool_name, access, Some(subject)))
                .collect::<Result<Vec<_>>>()?
        };

        Ok(PermissionDecision {
            mode: combine_modes(subject_modes),
            access,
            subjects,
            external_directory_required,
        })
    }

    fn decide_one_subject(
        &self,
        tool_name: &str,
        access: ToolAccess,
        subject: Option<&ToolSubject>,
    ) -> Result<ApprovalMode> {
        let mut mode = self
            .config
            .access
            .mode_for(access)
            .unwrap_or(self.config.default_mode);
        if let Some(tool_mode) = self.config.tools.get(tool_name).copied() {
            mode = tool_mode;
        }

        let matching_rule_modes = self
            .config
            .rules
            .iter()
            .filter(|rule| {
                rule.tool_name
                    .as_deref()
                    .is_none_or(|configured| configured == tool_name)
            })
            .filter_map(
                |rule| match rule_matches_subject(rule, tool_name, subject) {
                    Ok(true) => Some(Ok(rule.mode)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>>>()?;

        let tool_policy_mode = if matching_rule_modes.is_empty() {
            mode
        } else {
            combine_modes(matching_rule_modes)
        };

        let Some(subject) = subject else {
            return Ok(tool_policy_mode);
        };
        if subject.scope == ToolSubjectScope::External {
            Ok(combine_modes(vec![
                tool_policy_mode,
                self.decide_external_subject(subject)?,
            ]))
        } else {
            Ok(tool_policy_mode)
        }
    }

    fn decide_external_subject(&self, subject: &ToolSubject) -> Result<ApprovalMode> {
        let config = &self.config.external_directory;
        if !config.enabled {
            return Ok(ApprovalMode::Deny);
        }

        let matching_rule_modes = config
            .rules
            .iter()
            .filter_map(|rule| match external_rule_matches_subject(rule, subject) {
                Ok(true) => Some(Ok(rule.mode)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>>>()?;

        if matching_rule_modes.is_empty() {
            Ok(config.default_mode)
        } else {
            Ok(combine_modes(matching_rule_modes))
        }
    }
}

fn rule_matches_subject(
    rule: &PermissionRule,
    tool_name: &str,
    subject: Option<&ToolSubject>,
) -> Result<bool> {
    let Some(subject_glob) = &rule.subject_glob else {
        return Ok(true);
    };
    let subject_ref = subject
        .map(|subject| subject.normalized.as_str())
        .ok_or_else(|| anyhow!("permission rule requires a subject for {tool_name}"))?;
    let matcher = Glob::new(subject_glob)
        .map_err(|error| anyhow!("invalid permission glob {subject_glob}: {error}"))?
        .compile_matcher();
    Ok(matcher.is_match(subject_ref))
}

fn external_rule_matches_subject(
    rule: &ExternalDirectoryRule,
    subject: &ToolSubject,
) -> Result<bool> {
    let Some(canonical_path) = subject.canonical_path.as_ref() else {
        return Ok(false);
    };
    let pattern = canonical_external_rule_pattern(&rule.path_glob)?;
    let matcher = Glob::new(&pattern)
        .map_err(|error| {
            anyhow!(
                "invalid external directory glob {}: {error}",
                rule.path_glob
            )
        })?
        .compile_matcher();
    Ok(matcher.is_match(canonical_path))
}

fn canonical_external_rule_pattern(path_glob: &str) -> Result<String> {
    let expanded = expand_external_rule_path(path_glob)?;
    reject_parent_components(&expanded, "external directory path_glob")?;
    let expanded_path = Path::new(&expanded);
    if !expanded_path.is_absolute() {
        return Err(anyhow!(
            "external directory path_glob must be absolute, ~/..., or $HOME/..."
        ));
    }

    let mut literal_prefix = PathBuf::new();
    let mut glob_suffix = PathBuf::new();
    let mut in_glob_suffix = false;
    for component in expanded_path.components() {
        let part = component.as_os_str().to_string_lossy();
        if !in_glob_suffix && !contains_glob_token(&part) {
            literal_prefix.push(component.as_os_str());
        } else {
            in_glob_suffix = true;
            glob_suffix.push(component.as_os_str());
        }
    }

    if literal_prefix.as_os_str().is_empty() {
        literal_prefix.push(Path::new("/"));
    }
    let canonical_prefix = std::fs::canonicalize(&literal_prefix).map_err(|error| {
        anyhow!(
            "external directory literal prefix {} is not available: {error}",
            literal_prefix.display()
        )
    })?;
    let pattern = if glob_suffix.as_os_str().is_empty() {
        canonical_prefix
    } else {
        canonical_prefix.join(glob_suffix)
    };
    Ok(pattern.to_string_lossy().to_string())
}

fn expand_external_rule_path(path_glob: &str) -> Result<String> {
    let expanded = if path_glob == "~" {
        home_dir()?.to_string_lossy().to_string()
    } else if let Some(rest) = path_glob.strip_prefix("~/") {
        home_dir()?.join(rest).to_string_lossy().to_string()
    } else if path_glob == "$HOME" {
        home_dir()?.to_string_lossy().to_string()
    } else if let Some(rest) = path_glob.strip_prefix("$HOME/") {
        home_dir()?.join(rest).to_string_lossy().to_string()
    } else {
        path_glob.to_owned()
    };
    if expanded.contains('$') {
        return Err(anyhow!(
            "external directory path_glob only supports $HOME expansion"
        ));
    }
    Ok(expanded)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set for external directory path expansion"))
}

fn reject_parent_components(path: &str, label: &str) -> Result<()> {
    if Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(anyhow!("{label} must not contain .. components"));
    }
    Ok(())
}

fn contains_glob_token(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[')
}

pub fn combine_modes(modes: Vec<ApprovalMode>) -> ApprovalMode {
    if modes.iter().any(|mode| matches!(mode, ApprovalMode::Deny)) {
        ApprovalMode::Deny
    } else if modes.iter().any(|mode| matches!(mode, ApprovalMode::Ask)) {
        ApprovalMode::Ask
    } else {
        ApprovalMode::Allow
    }
}

#[cfg(test)]
#[path = "tests/permission_tests.rs"]
mod tests;
