use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use sigil_kernel::{
    AgentInvocationPolicy, AgentProfileKind, AgentResultPolicy, AgentTrustState, ReasoningEffort,
    ToolRegistryScope,
};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct NativeAgentProfileWire {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) kind: Option<AgentProfileKind>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) instructions: Option<String>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) provider: Option<String>,
    #[serde(default)]
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub(super) tool_scope: Option<ToolRegistryScope>,
    #[serde(default)]
    pub(super) allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub(super) tools: Option<Vec<String>>,
    #[serde(default)]
    pub(super) invocation_policy: Option<AgentInvocationPolicy>,
    #[serde(default)]
    pub(super) result_policy: Option<AgentResultPolicy>,
    #[serde(default)]
    pub(super) enabled: Option<bool>,
    #[serde(default)]
    pub(super) trust: Option<AgentTrustState>,
    #[serde(default)]
    pub(super) trust_state: Option<AgentTrustState>,
    #[serde(default)]
    pub(super) user_invocable: Option<bool>,
    #[serde(default)]
    pub(super) model_invocable: Option<bool>,
    #[serde(default)]
    pub(super) skills: Option<Vec<String>>,
    #[serde(default)]
    pub(super) mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    pub(super) nickname_candidates: Option<Vec<String>>,
    #[serde(default)]
    pub(super) aliases: Option<Vec<String>>,
    #[serde(default)]
    pub(super) slash_names: Option<Vec<String>>,
}

pub(super) fn markdown_agent_profile_wire(
    raw: &str,
) -> Result<(NativeAgentProfileWire, Option<String>)> {
    let mut lines = raw.lines();
    let Some(first) = lines.next() else {
        return Ok((NativeAgentProfileWire::default(), None));
    };
    if first.trim_end_matches('\r') != "---" {
        return Ok((NativeAgentProfileWire::default(), Some(raw.to_owned())));
    }
    let mut frontmatter = Vec::new();
    let mut body = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end_matches('\r') == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line.trim_end_matches('\r').to_owned());
    }
    if !closed {
        bail!("unterminated agent frontmatter");
    }
    body.extend(lines.map(str::to_owned));
    let fields = parse_markdown_frontmatter_fields(&frontmatter)?;
    Ok((wire_from_frontmatter_fields(fields)?, Some(body.join("\n"))))
}

pub(super) fn markdown_body_without_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else {
        return raw;
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    if let Some((_, body)) = rest.split_once("\n---\n") {
        return body;
    }
    if let Some((_, body)) = rest.split_once("\r\n---\r\n") {
        return body;
    }
    raw
}

fn parse_markdown_frontmatter_fields(lines: &[String]) -> Result<BTreeMap<String, Vec<String>>> {
    let mut fields = BTreeMap::<String, Vec<String>>::new();
    let mut current_key: Option<String> = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ") {
            let Some(key) = current_key.as_ref() else {
                bail!("frontmatter list item without a key");
            };
            fields
                .entry(key.clone())
                .or_default()
                .push(strip_scalar_quotes(item.trim()).to_owned());
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            bail!("invalid frontmatter line {trimmed:?}");
        };
        let key = key.trim().replace('-', "_");
        current_key = Some(key.clone());
        let value = value.trim();
        if value.is_empty() {
            fields.entry(key).or_default();
        } else {
            fields.insert(key, parse_inline_values(value));
        }
    }
    Ok(fields)
}

fn wire_from_frontmatter_fields(
    fields: BTreeMap<String, Vec<String>>,
) -> Result<NativeAgentProfileWire> {
    let string = |key: &str| -> Option<String> {
        fields
            .get(key)
            .and_then(|values| values.first())
            .filter(|value| !value.trim().is_empty())
            .cloned()
    };
    let list = |key: &str| -> Option<Vec<String>> {
        fields.get(key).cloned().filter(|values| !values.is_empty())
    };
    Ok(NativeAgentProfileWire {
        id: string("id"),
        kind: string("kind")
            .map(|value| parse_agent_kind(&value))
            .transpose()?,
        description: string("description"),
        instructions: string("instructions"),
        model: string("model"),
        provider: string("provider"),
        reasoning_effort: string("reasoning_effort")
            .map(|value| parse_reasoning_effort(&value))
            .transpose()?,
        tool_scope: None,
        allowed_tools: list("allowed_tools"),
        tools: list("tools"),
        invocation_policy: string("invocation_policy")
            .map(|value| parse_invocation_policy(&value))
            .transpose()?,
        result_policy: string("result_policy")
            .map(|value| parse_result_policy(&value))
            .transpose()?,
        enabled: string("enabled")
            .map(|value| parse_bool(&value))
            .transpose()?,
        trust: string("trust")
            .map(|value| parse_trust_state(&value))
            .transpose()?,
        trust_state: string("trust_state")
            .map(|value| parse_trust_state(&value))
            .transpose()?,
        user_invocable: string("user_invocable")
            .map(|value| parse_bool(&value))
            .transpose()?,
        model_invocable: string("model_invocable")
            .map(|value| parse_bool(&value))
            .transpose()?,
        skills: list("skills"),
        mcp_servers: list("mcp_servers"),
        nickname_candidates: list("nickname_candidates"),
        aliases: list("aliases").or_else(|| list("alias")),
        slash_names: list("slash_names").or_else(|| list("slash_name")),
    })
}

pub(super) fn parse_bool(value: &str) -> Result<bool> {
    match normalized_scalar(value).as_str() {
        "true" | "yes" => Ok(true),
        "false" | "no" => Ok(false),
        other => Err(anyhow!("invalid boolean value {other:?}")),
    }
}

pub(super) fn parse_agent_kind(value: &str) -> Result<AgentProfileKind> {
    match normalized_scalar(value).as_str() {
        "primary" => Ok(AgentProfileKind::Primary),
        "subagent" | "child" | "agent" => Ok(AgentProfileKind::Subagent),
        "system" => Ok(AgentProfileKind::System),
        other => Err(anyhow!("invalid agent kind {other:?}")),
    }
}

pub(super) fn parse_invocation_policy(value: &str) -> Result<AgentInvocationPolicy> {
    match normalized_scalar(value).as_str() {
        "manual_only" | "manual" => Ok(AgentInvocationPolicy::ManualOnly),
        "model_allowed" | "model" => Ok(AgentInvocationPolicy::ModelAllowed),
        "system_only" | "system" => Ok(AgentInvocationPolicy::SystemOnly),
        other => Err(anyhow!("invalid invocation policy {other:?}")),
    }
}

pub(super) fn parse_result_policy(value: &str) -> Result<AgentResultPolicy> {
    match normalized_scalar(value).as_str() {
        "summary_only" => Ok(AgentResultPolicy::SummaryOnly),
        "summary_with_page_ref" | "summary" => Ok(AgentResultPolicy::SummaryWithPageRef),
        "artifact_only" | "artifact" => Ok(AgentResultPolicy::ArtifactOnly),
        "foreground_merge_required" | "foreground" => {
            Ok(AgentResultPolicy::ForegroundMergeRequired)
        }
        other => Err(anyhow!("invalid result policy {other:?}")),
    }
}

pub(super) fn parse_trust_state(value: &str) -> Result<AgentTrustState> {
    match normalized_scalar(value).as_str() {
        "trusted" | "trust" => Ok(AgentTrustState::Trusted),
        "needs_review" | "review" => Ok(AgentTrustState::NeedsReview),
        "disabled" | "disable" => Ok(AgentTrustState::Disabled),
        other => Err(anyhow!("invalid trust state {other:?}")),
    }
}

pub(super) fn parse_reasoning_effort(value: &str) -> Result<ReasoningEffort> {
    match normalized_scalar(value).as_str() {
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "max" => Ok(ReasoningEffort::Max),
        other => Err(anyhow!("invalid reasoning effort {other:?}")),
    }
}

fn parse_inline_values(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inner
            .split(',')
            .map(|item| strip_scalar_quotes(item.trim()).to_owned())
            .filter(|item| !item.is_empty())
            .collect();
    }
    vec![strip_scalar_quotes(trimmed).to_owned()]
}

fn strip_scalar_quotes(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}

fn normalized_scalar(value: &str) -> String {
    strip_scalar_quotes(value)
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
}
