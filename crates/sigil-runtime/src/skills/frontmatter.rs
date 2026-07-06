use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    SkillDescriptor, SkillRunMode, SkillSource, SkillTrustState, ToolRegistryScope,
};

use super::{
    discovery::{SkillCandidateKind, valid_skill_id},
    namespaced_plugin_skill_id,
};

#[derive(Debug, Clone, Default)]
struct SkillFrontmatter {
    fields: BTreeMap<String, FrontmatterField>,
}

impl SkillFrontmatter {
    fn parse(raw: &str) -> Result<Self> {
        let mut lines = raw.lines();
        let Some(first) = lines.next() else {
            return Ok(Self::default());
        };
        if first.trim_end_matches('\r') != "---" {
            return Ok(Self::default());
        }

        let mut frontmatter_lines = Vec::new();
        let mut closed = false;
        for line in lines {
            if line.trim_end_matches('\r') == "---" {
                closed = true;
                break;
            }
            frontmatter_lines.push(line.trim_end_matches('\r').to_owned());
        }
        if !closed {
            bail!("unterminated skill frontmatter");
        }

        let fields = parse_frontmatter_fields(&frontmatter_lines)?;
        Ok(Self { fields })
    }

    fn to_descriptor(
        &self,
        id: String,
        root: &Path,
        entrypoint: &Path,
        fallback_id: &str,
        sha256: String,
        kind: &SkillCandidateKind,
        workspace_root: &Path,
    ) -> Result<SkillDescriptor> {
        let name = self
            .string("name")?
            .or(self.string("id")?)
            .unwrap_or_else(|| fallback_id.to_owned());
        let model_invocable = !self.bool("disable_model_invocation")?.unwrap_or(false);
        let allowed_tools = self
            .string_list("allowed_tools")?
            .or(self.string_list("tools")?)
            .unwrap_or_default();

        Ok(SkillDescriptor {
            id,
            name,
            description: self.string("description")?.unwrap_or_default(),
            when_to_use: self.string("when_to_use")?,
            root: display_path(workspace_root, root),
            entrypoint: display_path(workspace_root, entrypoint),
            source: kind.source(),
            sha256,
            enabled: self.bool("enabled")?.unwrap_or(true),
            trust: self.trust_state()?.unwrap_or_default(),
            model_invocable,
            user_invocable: self.bool("user_invocable")?.unwrap_or(true),
            run_as: self.run_mode()?.unwrap_or_else(|| kind.default_run_mode()),
            agent: self.string("agent")?,
            argument_hint: self.string("argument_hint")?,
            allowed_tools: tool_scope_from_items(allowed_tools),
            disallowed_tools: tool_scope_from_items(
                self.string_list("disallowed_tools")?.unwrap_or_default(),
            ),
            path_patterns: self.string_list("paths")?.unwrap_or_default(),
        })
    }

    fn string(&self, key: &str) -> Result<Option<String>> {
        let Some(field) = self.fields.get(key) else {
            return Ok(None);
        };
        field
            .value
            .clone()
            .filter(|value| !value.trim().is_empty())
            .map(Ok)
            .transpose()
    }

    fn bool(&self, key: &str) -> Result<Option<bool>> {
        self.string(key)?
            .map(|value| match normalized_scalar(&value).as_str() {
                "true" | "yes" => Ok(true),
                "false" | "no" => Ok(false),
                other => Err(anyhow!("invalid boolean value {other:?} for {key}")),
            })
            .transpose()
    }

    fn string_list(&self, key: &str) -> Result<Option<Vec<String>>> {
        let Some(field) = self.fields.get(key) else {
            return Ok(None);
        };
        if !field.list.is_empty() {
            return Ok(Some(field.list.clone()));
        }
        let Some(value) = field.value.as_deref() else {
            return Ok(Some(Vec::new()));
        };
        parse_inline_list(value)
            .with_context(|| format!("invalid list value for {key}"))
            .map(Some)
    }

    fn run_mode(&self) -> Result<Option<SkillRunMode>> {
        self.string("run_as")?
            .map(|value| match normalized_scalar(&value).as_str() {
                "inline" => Ok(SkillRunMode::Inline),
                "child_session" | "child" | "subagent" | "agent" => Ok(SkillRunMode::ChildSession),
                other => Err(anyhow!("invalid run-as value {other:?}")),
            })
            .transpose()
    }

    fn trust_state(&self) -> Result<Option<SkillTrustState>> {
        self.string("trust")?
            .map(|value| match normalized_scalar(&value).as_str() {
                "trusted" | "trust" => Ok(SkillTrustState::Trusted),
                "needs_review" | "review" => Ok(SkillTrustState::NeedsReview),
                "disabled" | "disable" => Ok(SkillTrustState::Disabled),
                other => Err(anyhow!("invalid trust value {other:?}")),
            })
            .transpose()
    }
}

#[derive(Debug, Clone, Default)]
struct FrontmatterField {
    value: Option<String>,
    list: Vec<String>,
}

fn parse_frontmatter_fields(lines: &[String]) -> Result<BTreeMap<String, FrontmatterField>> {
    let mut fields = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || line.starts_with(' ')
            || line.starts_with('\t')
        {
            index += 1;
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            bail!("unsupported frontmatter line {line:?}");
        };
        let key = normalize_key(raw_key);
        if key.is_empty() {
            bail!("empty frontmatter key");
        }
        let value = raw_value.trim();
        if value == ">" || value == "|" {
            bail!("unsupported multiline frontmatter value for {key}");
        }
        if value.is_empty() {
            let mut list = Vec::new();
            index += 1;
            let expects_list = list_frontmatter_key(&key);
            while index < lines.len() {
                let nested = &lines[index];
                if nested.trim().is_empty() {
                    index += 1;
                    continue;
                }
                if !nested.starts_with(' ') && !nested.starts_with('\t') {
                    break;
                }
                let nested_trimmed = nested.trim_start();
                if let Some(item) = nested_trimmed.strip_prefix("- ") {
                    list.push(clean_scalar(item)?);
                } else if expects_list {
                    bail!("unsupported list item for {key}: {nested_trimmed:?}");
                }
                index += 1;
            }
            fields.insert(key, FrontmatterField { value: None, list });
            continue;
        }

        fields.insert(
            key,
            FrontmatterField {
                value: Some(clean_scalar(value)?),
                list: Vec::new(),
            },
        );
        index += 1;
    }
    Ok(fields)
}

fn descriptor_id(
    frontmatter: &SkillFrontmatter,
    fallback_id: &str,
    kind: &SkillCandidateKind,
) -> Result<String> {
    let base_id = frontmatter
        .string("id")?
        .or(frontmatter.string("name")?)
        .unwrap_or_else(|| fallback_id.to_owned());
    if !valid_skill_id(&base_id) {
        bail!("invalid skill id {base_id:?}");
    }
    if let SkillSource::Plugin { plugin_id } = kind.source() {
        return namespaced_plugin_skill_id(&plugin_id, &base_id);
    }
    Ok(base_id)
}

pub(super) fn descriptor_from_entrypoint(
    workspace_root: &Path,
    root: &Path,
    entrypoint: &Path,
    fallback_id: &str,
    kind: &SkillCandidateKind,
) -> Result<SkillDescriptor> {
    let bytes = fs::read(entrypoint)
        .with_context(|| format!("failed to read skill entrypoint {}", entrypoint.display()))?;
    let raw = std::str::from_utf8(&bytes)
        .with_context(|| format!("skill entrypoint is not utf-8: {}", entrypoint.display()))?;
    let frontmatter = SkillFrontmatter::parse(raw)?;
    let id = descriptor_id(&frontmatter, fallback_id, kind)?;
    frontmatter.to_descriptor(
        id,
        root,
        entrypoint,
        fallback_id,
        format!("{:x}", Sha256::digest(&bytes)),
        kind,
        workspace_root,
    )
}

pub(super) fn fallback_skill_id(path: &Path) -> Result<String> {
    if path.file_name() == Some(OsStr::new("SKILL.md"))
        && let Some(parent_name) = path.parent().and_then(Path::file_name)
    {
        let value = parent_name.to_string_lossy().into_owned();
        if valid_skill_id(&value) {
            return Ok(value);
        }
        bail!("invalid plugin skill directory name {value:?}");
    }
    let value = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if valid_skill_id(&value) {
        Ok(value)
    } else {
        bail!("invalid plugin skill file name {value:?}")
    }
}

fn display_path(workspace_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(workspace_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_key(raw_key: &str) -> String {
    let normalized = raw_key.trim().replace('-', "_").to_ascii_lowercase();
    match normalized.as_str() {
        "runas" => "run_as".to_owned(),
        "disablemodelinvocation" => "disable_model_invocation".to_owned(),
        "userinvocable" => "user_invocable".to_owned(),
        "allowedtools" => "allowed_tools".to_owned(),
        "disallowedtools" => "disallowed_tools".to_owned(),
        "whentouse" => "when_to_use".to_owned(),
        _ => normalized,
    }
}

fn list_frontmatter_key(key: &str) -> bool {
    matches!(
        key,
        "allowed_tools" | "tools" | "disallowed_tools" | "paths"
    )
}

fn normalized_scalar(value: &str) -> String {
    value.trim().replace('-', "_").to_ascii_lowercase()
}

pub(super) fn clean_scalar(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed == ">" || trimmed == "|" {
        bail!("unsupported multiline scalar");
    }
    let unquoted = if quoted_scalar(trimmed) {
        &trimmed[1..trimmed.len() - 1]
    } else {
        strip_comment(trimmed).trim()
    };
    Ok(unquoted.trim().to_owned())
}

fn quoted_scalar(value: &str) -> bool {
    value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
}

fn strip_comment(value: &str) -> &str {
    value
        .split_once(" #")
        .map(|(value, _comment)| value)
        .unwrap_or(value)
}

pub(super) fn parse_inline_list(value: &str) -> Result<Vec<String>> {
    let trimmed = value.trim();
    let inner = if trimmed.starts_with('[') {
        if !trimmed.ends_with(']') {
            bail!("unterminated bracket list");
        }
        &trimmed[1..trimmed.len().saturating_sub(1)]
    } else {
        trimmed
    };
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(inner
        .split(',')
        .map(clean_scalar)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|item| !item.is_empty())
        .collect())
}

fn tool_scope_from_items(items: Vec<String>) -> ToolRegistryScope {
    let mut allow_all = false;
    let mut names = BTreeSet::new();
    let mut prefixes = Vec::new();
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "*" || trimmed.eq_ignore_ascii_case("all") {
            allow_all = true;
        } else if let Some(prefix) = trimmed.strip_suffix('*') {
            prefixes.push(prefix.to_owned());
        } else {
            names.insert(trimmed.to_owned());
        }
    }
    ToolRegistryScope {
        allow_all,
        names,
        prefixes,
    }
}
