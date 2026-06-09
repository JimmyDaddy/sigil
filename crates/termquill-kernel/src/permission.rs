use anyhow::{Result, anyhow};
use globset::Glob;
use serde::{Deserialize, Serialize};

use crate::tool::ToolSpec;

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

/// One explicit tool permission override rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionRule {
    pub tool_name: String,
    #[serde(default)]
    pub subject_glob: Option<String>,
    #[serde(default)]
    pub mode: ApprovalMode,
}

/// Shared permission policy configuration for one entrypoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PermissionConfig {
    #[serde(default)]
    pub write_mode: ApprovalMode,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            write_mode: ApprovalMode::Ask,
            rules: Vec::new(),
        }
    }
}

/// One resolved permission decision for a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub mode: ApprovalMode,
    pub subject: Option<String>,
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

    /// Resolves one tool call decision from the tool spec, stable name, and optional subject.
    ///
    /// # Errors
    ///
    /// Returns an error when one configured subject glob is invalid.
    pub fn decide(
        &self,
        spec: &ToolSpec,
        tool_name: &str,
        subject: Option<String>,
    ) -> Result<PermissionDecision> {
        let mut mode = if spec.read_only {
            ApprovalMode::Allow
        } else {
            self.config.write_mode
        };

        for rule in &self.config.rules {
            if rule.tool_name != tool_name {
                continue;
            }
            if let Some(subject_glob) = &rule.subject_glob {
                let subject_ref = subject
                    .as_deref()
                    .ok_or_else(|| anyhow!("permission rule requires a subject for {tool_name}"))?;
                let matcher = Glob::new(subject_glob)
                    .map_err(|error| anyhow!("invalid permission glob {subject_glob}: {error}"))?
                    .compile_matcher();
                if !matcher.is_match(subject_ref) {
                    continue;
                }
            }
            mode = rule.mode;
        }

        Ok(PermissionDecision { mode, subject })
    }
}

#[cfg(test)]
#[path = "tests/permission_tests.rs"]
mod tests;
