use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, de};
use sigil_kernel::{
    AgentInvocationPolicy, AgentProfileKind, AgentResultPolicy, AgentTrustState, ApprovalMode,
    CommandPermissionConfig, ExternalDirectoryConfig, ExternalDirectoryRule, PermissionConfig,
    PermissionRule, ReasoningEffort, ToolRegistryScope,
};

use crate::{LOAD_SKILL_TOOL_NAME, SPAWN_AGENT_TOOL_NAME};

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
    pub(super) permission: Option<AgentPermissionWire>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentPermissionWire {
    Action(ApprovalMode),
    Rules(Vec<AgentPermissionRuleWire>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentPermissionRuleWire {
    key: String,
    value: AgentPermissionValueWire,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentPermissionValueWire {
    Action(ApprovalMode),
    Patterns(Vec<(String, ApprovalMode)>),
    CommandGroups(CommandPermissionConfig),
}

impl<'de> Deserialize<'de> for AgentPermissionWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        agent_permission_wire_from_toml_value(&value).map_err(de::Error::custom)
    }
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
    Ok((
        wire_from_frontmatter_fields(fields, &frontmatter)?,
        Some(body.join("\n")),
    ))
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
    frontmatter: &[String],
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
        permission: parse_markdown_permission_wire(frontmatter)?,
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

pub(super) fn resolve_agent_permission_config(
    root: &PermissionConfig,
    wire: Option<AgentPermissionWire>,
) -> Result<PermissionConfig> {
    let mut config = root.clone();
    let Some(wire) = wire else {
        return Ok(config);
    };
    apply_agent_permission_wire(&mut config, &wire)?;
    Ok(config)
}

fn apply_agent_permission_wire(
    config: &mut PermissionConfig,
    wire: &AgentPermissionWire,
) -> Result<()> {
    match wire {
        AgentPermissionWire::Action(mode) => {
            config.rules.push(PermissionRule {
                tool_name: Some("*".to_owned()),
                subject_glob: None,
                mode: *mode,
            });
        }
        AgentPermissionWire::Rules(rules) => {
            for rule in rules {
                apply_agent_permission_rule(config, rule)?;
            }
        }
    }
    Ok(())
}

fn apply_agent_permission_rule(
    config: &mut PermissionConfig,
    rule: &AgentPermissionRuleWire,
) -> Result<()> {
    let key = normalized_permission_key(&rule.key);
    if key == "external_directory" {
        apply_external_directory_permission(config, &rule.value);
        return Ok(());
    }
    if key == "commands" {
        apply_command_permission(config, &rule.value)?;
        return Ok(());
    }
    let tools = permission_key_tools(&key);
    match &rule.value {
        AgentPermissionValueWire::Action(mode) => {
            for tool_name in tools {
                config.rules.push(PermissionRule {
                    tool_name: Some(tool_name),
                    subject_glob: None,
                    mode: *mode,
                });
            }
        }
        AgentPermissionValueWire::Patterns(patterns) => {
            for (pattern, mode) in patterns {
                for tool_name in &tools {
                    config.rules.push(PermissionRule {
                        tool_name: Some(tool_name.clone()),
                        subject_glob: permission_subject_glob(pattern),
                        mode: *mode,
                    });
                }
            }
        }
        AgentPermissionValueWire::CommandGroups(_) => {
            bail!("permission.commands must use allow/ask/deny groups");
        }
    }
    Ok(())
}

fn apply_external_directory_permission(
    config: &mut PermissionConfig,
    value: &AgentPermissionValueWire,
) {
    match value {
        AgentPermissionValueWire::Action(mode) => {
            config.external_directory.enabled = true;
            config.external_directory.default_mode = *mode;
        }
        AgentPermissionValueWire::Patterns(patterns) => {
            config.external_directory.enabled = true;
            if config.external_directory == ExternalDirectoryConfig::default() {
                config.external_directory.default_mode = ApprovalMode::Ask;
            }
            config
                .external_directory
                .rules
                .extend(
                    patterns
                        .iter()
                        .map(|(path_glob, mode)| ExternalDirectoryRule {
                            path_glob: path_glob.clone(),
                            mode: *mode,
                        }),
                );
        }
        AgentPermissionValueWire::CommandGroups(_) => {}
    }
}

fn apply_command_permission(
    config: &mut PermissionConfig,
    value: &AgentPermissionValueWire,
) -> Result<()> {
    let AgentPermissionValueWire::CommandGroups(command_config) = value else {
        bail!("permission.commands must use allow/ask/deny groups");
    };
    let mut merged = config.commands.clone();
    merged
        .extend_from(command_config)
        .context("invalid permission.commands")?;
    config.commands = merged;
    Ok(())
}

fn permission_subject_glob(pattern: &str) -> Option<String> {
    let normalized = pattern.trim();
    if normalized == "*" {
        None
    } else {
        Some(normalized.to_owned())
    }
}

fn normalized_permission_key(key: &str) -> String {
    key.trim().replace('-', "_").to_ascii_lowercase()
}

fn permission_key_tools(key: &str) -> Vec<String> {
    match key {
        "read" => vec!["read_file".to_owned()],
        "edit" | "write" => ["write_file", "edit_file", "delete_file", "apply_changeset"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "glob" => vec!["glob".to_owned()],
        "grep" => vec!["grep".to_owned()],
        "list" => vec!["ls".to_owned()],
        "bash" => vec!["bash".to_owned()],
        "task" => vec![SPAWN_AGENT_TOOL_NAME.to_owned()],
        "skill" => vec![LOAD_SKILL_TOOL_NAME.to_owned()],
        "webfetch" => vec!["webfetch".to_owned()],
        "websearch" => vec!["websearch".to_owned()],
        "lsp" => [
            "code_symbols",
            "code_workspace_symbols",
            "code_definition",
            "code_references",
            "code_diagnostics",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        other => vec![other.to_owned()],
    }
}

fn agent_permission_wire_from_toml_value(value: &toml::Value) -> Result<AgentPermissionWire> {
    match value {
        toml::Value::String(action) => {
            Ok(AgentPermissionWire::Action(parse_approval_mode(action)?))
        }
        toml::Value::Table(table) => table
            .iter()
            .map(|(key, value)| {
                Ok(AgentPermissionRuleWire {
                    key: key.clone(),
                    value: agent_permission_value_from_toml_value(key, value)?,
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(AgentPermissionWire::Rules),
        other => bail!("invalid agent permission value {other:?}"),
    }
}

fn agent_permission_value_from_toml_value(
    key: &str,
    value: &toml::Value,
) -> Result<AgentPermissionValueWire> {
    if normalized_permission_key(key) == "commands" {
        return parse_toml_command_permission_config(value)
            .map(AgentPermissionValueWire::CommandGroups);
    }
    match value {
        toml::Value::String(action) => Ok(AgentPermissionValueWire::Action(parse_approval_mode(
            action,
        )?)),
        toml::Value::Table(table) => table
            .iter()
            .map(|(pattern, value)| {
                let toml::Value::String(action) = value else {
                    bail!("invalid agent permission pattern action for {key}.{pattern}");
                };
                Ok((pattern.clone(), parse_approval_mode(action)?))
            })
            .collect::<Result<Vec<_>>>()
            .map(AgentPermissionValueWire::Patterns),
        other => bail!("invalid agent permission value for {key}: {other:?}"),
    }
}

fn parse_toml_command_permission_config(value: &toml::Value) -> Result<CommandPermissionConfig> {
    value
        .clone()
        .try_into::<CommandPermissionConfig>()
        .context("invalid permission.commands")
}

fn parse_markdown_permission_wire(lines: &[String]) -> Result<Option<AgentPermissionWire>> {
    let Some((index, value)) = lines.iter().enumerate().find_map(|(index, line)| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("permission:")
            .map(|value| (index, value.trim()))
    }) else {
        return Ok(None);
    };
    if !value.is_empty() {
        return Ok(Some(AgentPermissionWire::Action(parse_approval_mode(
            value,
        )?)));
    }

    let mut rules = Vec::new();
    let mut cursor = index + 1;
    while cursor < lines.len() {
        let line = &lines[cursor];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            cursor += 1;
            continue;
        }
        let indent = leading_space_count(line);
        if indent == 0 {
            break;
        }
        let trimmed = line.trim();
        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            bail!("invalid permission frontmatter line {trimmed:?}");
        };
        let key = strip_scalar_quotes(raw_key.trim()).to_owned();
        let raw_value = raw_value.trim();
        if normalized_permission_key(&key) == "commands" {
            if !raw_value.is_empty() {
                bail!("permission.commands must use allow/ask/deny groups");
            }
            let (commands, next_cursor) =
                parse_markdown_permission_commands_block(lines, cursor + 1, indent)?;
            rules.push(AgentPermissionRuleWire {
                key,
                value: AgentPermissionValueWire::CommandGroups(commands),
            });
            cursor = next_cursor;
            continue;
        }
        if !raw_value.is_empty() {
            rules.push(AgentPermissionRuleWire {
                key,
                value: AgentPermissionValueWire::Action(parse_approval_mode(raw_value)?),
            });
            cursor += 1;
            continue;
        }
        let (patterns, next_cursor) =
            parse_markdown_permission_pattern_block(lines, cursor + 1, indent)?;
        rules.push(AgentPermissionRuleWire {
            key,
            value: AgentPermissionValueWire::Patterns(patterns),
        });
        cursor = next_cursor;
    }

    Ok(Some(AgentPermissionWire::Rules(rules)))
}

fn parse_markdown_permission_pattern_block(
    lines: &[String],
    mut cursor: usize,
    parent_indent: usize,
) -> Result<(Vec<(String, ApprovalMode)>, usize)> {
    let mut patterns = Vec::new();
    while cursor < lines.len() {
        let line = &lines[cursor];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            cursor += 1;
            continue;
        }
        let indent = leading_space_count(line);
        if indent <= parent_indent {
            break;
        }
        let trimmed = line.trim();
        let Some((raw_pattern, raw_action)) = trimmed.split_once(':') else {
            bail!("invalid permission pattern line {trimmed:?}");
        };
        let action = raw_action.trim();
        if action.is_empty() {
            bail!("missing permission action for pattern {raw_pattern:?}");
        }
        patterns.push((
            strip_scalar_quotes(raw_pattern.trim()).to_owned(),
            parse_approval_mode(action).with_context(|| {
                format!("invalid permission action for pattern {raw_pattern:?}")
            })?,
        ));
        cursor += 1;
    }
    Ok((patterns, cursor))
}

fn parse_markdown_permission_commands_block(
    lines: &[String],
    mut cursor: usize,
    parent_indent: usize,
) -> Result<(CommandPermissionConfig, usize)> {
    let mut config = CommandPermissionConfig::default();
    while cursor < lines.len() {
        let line = &lines[cursor];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            cursor += 1;
            continue;
        }
        let indent = leading_space_count(line);
        if indent <= parent_indent {
            break;
        }
        let trimmed = line.trim();
        let Some((raw_group, raw_value)) = trimmed.split_once(':') else {
            bail!("invalid permission.commands line {trimmed:?}");
        };
        let group = normalized_permission_key(strip_scalar_quotes(raw_group.trim()));
        let raw_value = raw_value.trim();
        let (patterns, next_cursor) = if raw_value.is_empty() {
            parse_markdown_command_pattern_list(lines, cursor + 1, indent)?
        } else {
            (parse_inline_values(raw_value), cursor + 1)
        };
        match group.as_str() {
            "allow" => config.allow.extend(patterns),
            "ask" => config.ask.extend(patterns),
            "deny" => config.deny.extend(patterns),
            other => bail!("unknown permission.commands group {other:?}"),
        }
        cursor = next_cursor;
    }
    let mut validated = CommandPermissionConfig::default();
    validated
        .extend_from(&config)
        .context("invalid permission.commands")?;
    Ok((validated, cursor))
}

fn parse_markdown_command_pattern_list(
    lines: &[String],
    mut cursor: usize,
    parent_indent: usize,
) -> Result<(Vec<String>, usize)> {
    let mut patterns = Vec::new();
    while cursor < lines.len() {
        let line = &lines[cursor];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            cursor += 1;
            continue;
        }
        let indent = leading_space_count(line);
        if indent <= parent_indent {
            break;
        }
        let trimmed = line.trim();
        let Some(item) = trimmed.strip_prefix("- ") else {
            bail!("permission.commands patterns must be a list");
        };
        patterns.push(strip_scalar_quotes(item.trim()).to_owned());
        cursor += 1;
    }
    Ok((patterns, cursor))
}

fn leading_space_count(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn parse_approval_mode(value: &str) -> Result<ApprovalMode> {
    match normalized_scalar(value).as_str() {
        "allow" => Ok(ApprovalMode::Allow),
        "ask" => Ok(ApprovalMode::Ask),
        "deny" => Ok(ApprovalMode::Deny),
        other => Err(anyhow!("invalid permission action {other:?}")),
    }
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
