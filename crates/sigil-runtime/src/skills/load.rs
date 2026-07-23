use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ControlEntry, ModelMessage, SkillConfig, SkillDescriptor, SkillIndexSnapshot, SkillLoadEntry,
    SkillTrustState, Tool, ToolAccess, ToolCategory, ToolContext, ToolErrorKind,
    ToolMutationTracking, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
};

use super::{LOAD_SKILL_TOOL_NAME, SkillDiscoveryReport, discover_skill_index_with_user_dir};

const MAX_SKILL_BODY_BYTES: usize = 256 * 1024;
const MAX_SKILL_BODY_LINES: usize = 8_000;
const MAX_MODEL_VISIBLE_SKILLS: usize = 80;
const MAX_MODEL_VISIBLE_INDEX_BYTES: usize = 8 * 1024;

/// Fully loaded skill body prepared for direct user invocation.
#[derive(Debug, Clone)]
pub struct LoadedSkillContext {
    pub descriptor: SkillDescriptor,
    pub entry: SkillLoadEntry,
    pub transient_context: ModelMessage,
}

/// Registers the internal model-facing skill loading tool from a discovered index.
///
/// # Errors
///
/// Returns an error when skill discovery fails.
pub fn register_skill_tools(
    registry: &mut ToolRegistry,
    workspace_root: &Path,
    user_config_dir: Option<&Path>,
    config: &SkillConfig,
) -> Result<SkillDiscoveryReport> {
    let report = discover_skill_index_with_user_dir(workspace_root, user_config_dir, config)?;
    if config.enabled {
        registry.register(Arc::new(LoadSkillTool::new(
            workspace_root.to_path_buf(),
            report.snapshot.clone(),
        )));
    }
    Ok(report)
}

#[derive(Debug, Clone)]
pub(super) struct LoadSkillTool {
    workspace_root: PathBuf,
    snapshot: SkillIndexSnapshot,
}

impl LoadSkillTool {
    pub(super) fn new(workspace_root: PathBuf, snapshot: SkillIndexSnapshot) -> Self {
        Self {
            workspace_root,
            snapshot,
        }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: LOAD_SKILL_TOOL_NAME.to_owned(),
            description: model_visible_skill_index_description(&self.snapshot),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Stable id of a trusted model-invocable skill to load."
                    }
                },
                "required": ["id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::None
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![skill_subject(required_skill_id(args)?)])
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let skill_id = match required_skill_id(&args) {
            Ok(skill_id) => skill_id.to_owned(),
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    LOAD_SKILL_TOOL_NAME,
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                ));
            }
        };
        let loaded = match load_skill_context(
            &self.workspace_root,
            &self.snapshot,
            &skill_id,
            SkillLoadInvocation::Model,
            None,
            Some(call_id.clone()),
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                return Ok(ToolResult::error(
                    call_id,
                    LOAD_SKILL_TOOL_NAME,
                    error.kind,
                    error.message,
                ));
            }
        };
        Ok(ToolResult::ok(
            call_id,
            LOAD_SKILL_TOOL_NAME,
            format!(
                "loaded skill {} ({} bytes, {} lines) into transient context",
                loaded.entry.skill_id, loaded.entry.byte_count, loaded.entry.line_count
            ),
            ToolResultMeta {
                bytes: Some(loaded.entry.byte_count),
                total_lines: Some(loaded.entry.line_count),
                ..ToolResultMeta::default()
            },
        )
        .with_control_entry(ControlEntry::SkillLoaded(loaded.entry))
        .with_transient_context(vec![loaded.transient_context]))
    }
}

/// Loads a user-invoked skill body for direct TUI invocation.
///
/// # Errors
///
/// Returns an error when the skill is missing, disabled, untrusted, not user-invocable, changed
/// since discovery, outside its root, too large, or unreadable.
pub fn load_user_invoked_skill(
    workspace_root: &Path,
    snapshot: &SkillIndexSnapshot,
    skill_id: &str,
    run_id: Option<String>,
) -> Result<LoadedSkillContext> {
    load_skill_context(
        workspace_root,
        snapshot,
        skill_id,
        SkillLoadInvocation::User,
        run_id,
        None,
    )
    .map_err(|error| anyhow!(error.message))
}

#[derive(Debug, Clone, Copy)]
enum SkillLoadInvocation {
    Model,
    User,
}

#[derive(Debug)]
struct SkillLoadFailure {
    kind: ToolErrorKind,
    message: String,
}

fn load_skill_context(
    workspace_root: &Path,
    snapshot: &SkillIndexSnapshot,
    skill_id: &str,
    invocation: SkillLoadInvocation,
    run_id: Option<String>,
    call_id: Option<String>,
) -> std::result::Result<LoadedSkillContext, SkillLoadFailure> {
    let Some(descriptor) = snapshot
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id == skill_id)
    else {
        return Err(skill_load_failure(
            ToolErrorKind::NotFound,
            format!("unknown skill {skill_id:?}"),
        ));
    };
    if !descriptor.enabled {
        return Err(skill_load_failure(
            ToolErrorKind::PermissionDenied,
            format!("skill {skill_id:?} is disabled"),
        ));
    }
    if descriptor.trust != SkillTrustState::Trusted {
        return Err(skill_load_failure(
            ToolErrorKind::PermissionDenied,
            format!("skill {skill_id:?} is not trusted"),
        ));
    }
    match invocation {
        SkillLoadInvocation::Model if !descriptor.model_invocable => {
            return Err(skill_load_failure(
                ToolErrorKind::PermissionDenied,
                format!("skill {skill_id:?} is not model-invocable"),
            ));
        }
        SkillLoadInvocation::User if !descriptor.user_invocable => {
            return Err(skill_load_failure(
                ToolErrorKind::PermissionDenied,
                format!("skill {skill_id:?} is not user-invocable"),
            ));
        }
        SkillLoadInvocation::Model | SkillLoadInvocation::User => {}
    }

    let root = resolved_descriptor_path(workspace_root, &descriptor.root);
    let entrypoint = resolved_descriptor_path(workspace_root, &descriptor.entrypoint);
    let root = root.canonicalize().map_err(|error| {
        skill_load_failure(
            ToolErrorKind::NotFound,
            format!("skill root cannot be resolved: {error}"),
        )
    })?;
    let entrypoint = entrypoint.canonicalize().map_err(|error| {
        skill_load_failure(
            ToolErrorKind::NotFound,
            format!("skill entrypoint cannot be resolved: {error}"),
        )
    })?;
    if !entrypoint.starts_with(&root) {
        return Err(skill_load_failure(
            ToolErrorKind::PathOutsideWorkspace,
            format!("skill {skill_id:?} entrypoint is outside its skill root"),
        ));
    }

    let bytes = fs::read(&entrypoint).map_err(|error| {
        skill_load_failure(
            ToolErrorKind::Io,
            format!("failed to read skill {skill_id:?}: {error}"),
        )
    })?;
    if bytes.len() > MAX_SKILL_BODY_BYTES {
        return Err(skill_load_failure(
            ToolErrorKind::InvalidInput,
            format!(
                "skill {skill_id:?} body is too large: {} bytes exceeds {}",
                bytes.len(),
                MAX_SKILL_BODY_BYTES
            ),
        ));
    }
    let body = std::str::from_utf8(&bytes).map_err(|error| {
        skill_load_failure(
            ToolErrorKind::Utf8,
            format!("skill {skill_id:?} is not utf-8: {error}"),
        )
    })?;
    let line_count = body.lines().count();
    if line_count > MAX_SKILL_BODY_LINES {
        return Err(skill_load_failure(
            ToolErrorKind::InvalidInput,
            format!(
                "skill {skill_id:?} body has too many lines: {line_count} exceeds {MAX_SKILL_BODY_LINES}"
            ),
        ));
    }
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    if !descriptor.sha256.is_empty() && descriptor.sha256 != sha256 {
        return Err(skill_load_failure(
            ToolErrorKind::InvalidInput,
            format!("skill {skill_id:?} hash changed since discovery"),
        ));
    }

    let entry = SkillLoadEntry {
        skill_id: descriptor.id.clone(),
        sha256,
        source: descriptor.source.clone(),
        entrypoint: descriptor.entrypoint.clone(),
        run_id,
        call_id,
        byte_count: bytes.len() as u64,
        line_count: line_count as u64,
        loaded_at_ms: unix_time_ms(),
    };
    let transient_context =
        ModelMessage::system(render_loaded_skill_context(descriptor, &entry, body));
    Ok(LoadedSkillContext {
        descriptor: descriptor.clone(),
        entry,
        transient_context,
    })
}

fn skill_load_failure(kind: ToolErrorKind, message: String) -> SkillLoadFailure {
    SkillLoadFailure { kind, message }
}

pub(super) fn model_visible_skill_index_description(snapshot: &SkillIndexSnapshot) -> String {
    let mut description = String::from(
        "Load one trusted reusable skill by id. The full skill body is loaded only on demand and is injected as transient context for the current run.\n\nAvailable skills:",
    );
    let mut rendered = 0usize;
    for descriptor in snapshot
        .descriptors
        .iter()
        .filter(|descriptor| model_visible_skill_descriptor(descriptor))
        .take(MAX_MODEL_VISIBLE_SKILLS)
    {
        let mut line = format!("\n- {}", descriptor.id);
        if !descriptor.description.trim().is_empty() {
            line.push_str(": ");
            line.push_str(descriptor.description.trim());
        }
        if let Some(when_to_use) = descriptor.when_to_use.as_deref()
            && !when_to_use.trim().is_empty()
        {
            line.push_str(" Use when: ");
            line.push_str(when_to_use.trim());
        }
        if description.len() + line.len() > MAX_MODEL_VISIBLE_INDEX_BYTES {
            description.push_str("\n- ...");
            return description;
        }
        description.push_str(&line);
        rendered += 1;
    }
    if rendered == 0 {
        description.push_str("\n- none");
    }
    description
}

fn model_visible_skill_descriptor(descriptor: &SkillDescriptor) -> bool {
    descriptor.enabled && descriptor.trust == SkillTrustState::Trusted && descriptor.model_invocable
}

fn required_skill_id(args: &Value) -> Result<&str> {
    args.get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing id"))
}

fn skill_subject(skill_id: &str) -> ToolSubject {
    ToolSubject {
        kind: ToolSubjectKind::Other,
        original: skill_id.to_owned(),
        normalized: format!("skill:{skill_id}"),
        canonical_path: None,
        scope: ToolSubjectScope::Unknown,
    }
}

pub(super) fn resolved_descriptor_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

fn render_loaded_skill_context(
    descriptor: &SkillDescriptor,
    entry: &SkillLoadEntry,
    body: &str,
) -> String {
    format!(
        "Loaded Sigil skill\nid: {}\nsource: {}\nrun_as: {}\nentrypoint: {}\nsha256: {}\n\n{}",
        descriptor.id,
        descriptor.source.as_str(),
        descriptor.run_as.as_str(),
        descriptor.entrypoint.display(),
        entry.sha256,
        body
    )
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
