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
mod tests {
    use anyhow::Result;
    use serde_json::json;

    use crate::ToolSpec;

    use super::{ApprovalMode, PermissionConfig, PermissionPolicy, PermissionRule};

    fn write_spec() -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: json!({"type":"object"}),
            read_only: false,
        }
    }

    #[test]
    fn permission_rules_override_default_write_mode() -> Result<()> {
        let config = PermissionConfig {
            write_mode: ApprovalMode::Ask,
            rules: vec![PermissionRule {
                tool_name: "write_file".to_owned(),
                subject_glob: None,
                mode: ApprovalMode::Deny,
            }],
        };
        let decision = PermissionPolicy::new(&config).decide(
            &write_spec(),
            "write_file",
            Some("src/main.rs".to_owned()),
        )?;

        assert_eq!(decision.mode, ApprovalMode::Deny);
        Ok(())
    }

    #[test]
    fn permission_rules_match_subject_glob() -> Result<()> {
        let config = PermissionConfig {
            write_mode: ApprovalMode::Ask,
            rules: vec![
                PermissionRule {
                    tool_name: "write_file".to_owned(),
                    subject_glob: Some("src/**".to_owned()),
                    mode: ApprovalMode::Allow,
                },
                PermissionRule {
                    tool_name: "write_file".to_owned(),
                    subject_glob: Some("src/**/*.md".to_owned()),
                    mode: ApprovalMode::Deny,
                },
            ],
        };
        let allow = PermissionPolicy::new(&config).decide(
            &write_spec(),
            "write_file",
            Some("src/main.rs".to_owned()),
        )?;
        let deny = PermissionPolicy::new(&config).decide(
            &write_spec(),
            "write_file",
            Some("src/docs/guide.md".to_owned()),
        )?;

        assert_eq!(allow.mode, ApprovalMode::Allow);
        assert_eq!(deny.mode, ApprovalMode::Deny);
        Ok(())
    }
}
