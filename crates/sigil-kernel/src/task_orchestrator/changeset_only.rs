use super::*;

const CHANGESET_ONLY_CHILD_TOOL_NAMES: &[&str] = &[
    "read_file",
    "ls",
    "glob",
    "grep",
    "code_symbols",
    "code_workspace_symbols",
    "code_definition",
    "code_references",
    "code_diagnostics",
    "load_skill",
];

/// Returns the only tool surface allowed for a `ChangesetOnly` child writer.
///
/// The child proposes a structured changeset through its final result. It must not execute
/// mutating tools such as `write_file`, `edit_file`, `delete_file`, `apply_changeset`, `bash`,
/// terminal tools, MCP tools, or plugin tools in the parent workspace.
pub fn changeset_only_child_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        CHANGESET_ONLY_CHILD_TOOL_NAMES.iter().copied(),
        std::iter::empty::<&'static str>(),
    )
}

/// Returns a capability-filtered tool registry for `ChangesetOnly` child writers.
///
/// The initial scope is name-based for provider schema stability, but this function also validates
/// the resolved tool specs so a replaced same-name tool cannot carry write/execute/network access.
pub fn changeset_only_child_tool_registry(registry: &ToolRegistry) -> ToolRegistry {
    let scoped = registry.scoped(changeset_only_child_tool_scope());
    let safe_names = scoped
        .specs()
        .into_iter()
        .filter(changeset_only_child_tool_spec_is_safe)
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    registry
        .scoped(ToolRegistryScope::from_names_and_prefixes(
            safe_names,
            std::iter::empty::<String>(),
        ))
        .into_registry()
}

pub(super) fn changeset_only_child_tool_spec_is_safe(spec: &ToolSpec) -> bool {
    spec.access == ToolAccess::Read
        && spec.network_effect.is_none()
        && matches!(
            spec.category,
            ToolCategory::File | ToolCategory::Search | ToolCategory::Custom
        )
}

/// Returns the provider-visible contract for a changeset-only child writer.
///
/// The prompt is shared by task children and foreground chat worker agents so both surfaces
/// produce the same reviewable changeset envelope without receiving parent-workspace write tools.
#[must_use]
pub fn changeset_only_child_contract_prompt() -> &'static str {
    r#"This delegated write step uses changeset-only isolation.

You must not modify files, run shell commands, use terminal tools, call apply_changeset, or call any MCP/plugin tool.

Return the proposed edit as structured JSON only. Use a raw JSON object or a fenced block tagged sigil_changeset. The schema is:

```sigil_changeset
{
  "change_set": {
    "id": "change-brief-stable-id",
    "title": "short user-facing title",
    "summary": "what the change would do",
    "risk": "low",
    "files": [
      {
        "path": "relative/path",
        "action": "update",
        "risk": "low",
        "additions": 0,
        "deletions": 0
      }
    ],
    "validations": []
  },
  "artifact": {
    "media_type": "text/x-diff",
    "content": "reviewable patch, diff, or exact change artifact content"
  }
}
```

Do not claim the changes were applied. They will be reviewed and applied by the parent session later."#
}

/// Decodes the strict structured output expected from a changeset-only child writer.
///
/// # Errors
///
/// Returns an error when the final output is not raw JSON or a `sigil_changeset` fenced JSON
/// block, or when the decoded changeset is empty or contains unsafe paths.
pub fn decode_changeset_only_child_output(final_text: &str) -> Result<TaskChildChangeSetProposal> {
    let json_text = extract_changeset_only_json(final_text).ok_or_else(|| {
        anyhow!("changeset-only child output must be raw JSON or a sigil_changeset fenced block")
    })?;
    let envelope: TaskChildChangeSetProposalEnvelope = serde_json::from_str(json_text)
        .map_err(|error| anyhow!("invalid changeset-only child output JSON: {error}"))?;
    let proposal = envelope.into_proposal()?;
    validate_changeset_only_proposal(&proposal.change_set)?;
    Ok(proposal)
}

#[derive(Deserialize)]
struct TaskChildChangeSetProposalEnvelope {
    change_set: ChangeSet,
    artifact: TaskChildChangeSetArtifactWire,
}

#[derive(Deserialize)]
struct TaskChildChangeSetArtifactWire {
    media_type: String,
    content: String,
}

impl TaskChildChangeSetProposalEnvelope {
    fn into_proposal(self) -> Result<TaskChildChangeSetProposal> {
        let media_type = self.artifact.media_type.trim();
        if media_type.is_empty() {
            bail!(
                "changeset-only proposal {} artifact media_type must be non-empty",
                self.change_set.id.as_str()
            );
        }
        let content = self.artifact.content;
        if content.trim().is_empty() {
            bail!(
                "changeset-only proposal {} artifact content must be non-empty",
                self.change_set.id.as_str()
            );
        }
        let content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        Ok(TaskChildChangeSetProposal {
            change_set: self.change_set,
            artifact_ref: format!("inline:sha256:{content_sha256}"),
            artifact: TaskChildChangeSetArtifact {
                media_type: media_type.to_owned(),
                content,
                content_sha256,
            },
            source_isolation: WriteIsolationMode::ChangesetOnly,
            child_snapshot_id: None,
            integration_facts: crate::IntegrationProposalFacts::default(),
        })
    }
}

pub(super) fn extract_changeset_only_json(final_text: &str) -> Option<&str> {
    let trimmed = final_text.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let marker = "```sigil_changeset";
    let start = trimmed.find(marker)? + marker.len();
    let after_marker = trimmed[start..]
        .strip_prefix("\r\n")
        .or_else(|| trimmed[start..].strip_prefix('\n'))
        .unwrap_or(&trimmed[start..]);
    let end = after_marker.find("```")?;
    Some(after_marker[..end].trim())
}

pub(super) fn validate_changeset_only_proposal(change_set: &ChangeSet) -> Result<()> {
    if change_set.files.is_empty() {
        bail!(
            "changeset-only proposal {} must include at least one touched file",
            change_set.id.as_str()
        );
    }
    for file in &change_set.files {
        validate_changeset_path(&file.path)?;
        if let Some(previous_path) = &file.previous_path {
            validate_changeset_path(previous_path)?;
        }
    }
    Ok(())
}

pub(super) fn validate_changeset_path(path: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("changeset proposal file path cannot be empty");
    }
    let path = Path::new(trimmed);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("changeset proposal file path must stay inside the workspace: {trimmed}");
    }
    Ok(())
}
