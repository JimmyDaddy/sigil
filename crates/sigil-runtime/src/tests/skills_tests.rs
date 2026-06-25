use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutput, ApprovalHandler, ApprovalMode,
    AutoApproveHandler, CompactionConfig, CompletionRequest, ControlEntry, InteractionMode,
    MemoryConfig, MessageRole, NoopEventHandler, PermissionConfig, Provider, ProviderCapabilities,
    ProviderChunk, ReasoningEffort, ReasoningStreamSupport, Session, SessionLogEntry, SkillConfig,
    SkillDescriptor, SkillIndexSnapshot, SkillRunMode, SkillSource, SkillTrustState, Tool,
    ToolApprovalAuditAction, ToolApprovalUserDecision, ToolCall, ToolContext, ToolErrorKind,
    ToolRegistry, ToolResultStatus,
};

use super::{
    LOAD_SKILL_TOOL_NAME, SkillDiscoveryWarningKind, discover_skill_index,
    discover_skill_index_with_project_assets_root, discover_skill_index_with_user_dir,
    namespaced_plugin_skill_id, register_skill_tools,
};

struct LoadSkillProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for LoadSkillProvider {
    fn name(&self) -> &str {
        "mock-load-skill"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_result_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        let chunks = if tool_result_seen {
            vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ]
        } else {
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-load-skill".to_owned(),
                    name: LOAD_SKILL_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-load-skill".to_owned(),
                    delta: r#"{"id":"repo-review"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-load-skill".to_owned(),
                    name: LOAD_SKILL_TOOL_NAME.to_owned(),
                    args_json: r#"{"id":"repo-review"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[test]
fn discovery_prefers_workspace_native_skills_over_compat_and_user_duplicates() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let user_config = tempfile::tempdir().expect("user config should create");
    write_skill(
        workspace.path().join(".sigil/skills/review/SKILL.md"),
        r#"---
name: review
description: Workspace review
when-to-use: Use for repository reviews.
allowed-tools: [read_file, grep]
---

# Review
"#,
    );
    write_skill(
        workspace.path().join(".claude/skills/review/SKILL.md"),
        r#"---
name: review
description: Claude review
---

# Review
"#,
    );
    write_skill(
        user_config.path().join("skills/review/SKILL.md"),
        r#"---
name: review
description: User review
---

# Review
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/deploy/SKILL.md"),
        r#"---
name: deploy
description: Deploy the workspace.
disable-model-invocation: true
---

# Deploy
"#,
    );

    let report = discover_skill_index_with_user_dir(
        workspace.path(),
        Some(user_config.path()),
        &SkillConfig::default(),
    )
    .expect("discovery should succeed");

    assert_eq!(
        report
            .snapshot
            .descriptors
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>(),
        vec!["deploy", "review"]
    );
    let review = descriptor(&report, "review");
    assert_eq!(review.description, "Workspace review");
    assert_eq!(review.source, SkillSource::Workspace);
    assert_eq!(
        review.when_to_use.as_deref(),
        Some("Use for repository reviews.")
    );
    assert!(review.allowed_tools.names.contains("read_file"));
    assert!(review.allowed_tools.names.contains("grep"));
    assert!(!review.sha256.is_empty());
    assert_ne!(report.snapshot.fingerprint, "none");
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| warning.kind == SkillDiscoveryWarningKind::Shadowed)
            .count(),
        2
    );
}

#[test]
fn discovery_uses_explicit_project_assets_root_for_workspace_skills_and_agents() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let project_assets = workspace.path().join("project-assets");
    write_skill(
        project_assets.join("skills/review/SKILL.md"),
        r#"---
name: review
description: Project asset review skill.
---

# Review
"#,
    );
    write_skill(
        project_assets.join("agents/audit.md"),
        r#"---
name: audit
description: Project asset agent skill.
---

# Audit
"#,
    );

    let report = discover_skill_index_with_project_assets_root(
        workspace.path(),
        &project_assets,
        None,
        &SkillConfig::default(),
    )
    .expect("discovery should succeed");

    assert_eq!(
        report
            .snapshot
            .descriptors
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>(),
        vec!["audit", "review"]
    );
    let review = descriptor(&report, "review");
    assert_eq!(review.source, SkillSource::Workspace);
    assert!(review.entrypoint.starts_with(Path::new("project-assets")));
    let audit = descriptor(&report, "audit");
    assert_eq!(audit.run_as, SkillRunMode::ChildSession);
    assert!(audit.entrypoint.starts_with(Path::new("project-assets")));
}

#[test]
fn claude_agents_are_discovered_as_child_session_skills() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".claude/agents/reviewer.md"),
        r#"---
name: reviewer
description: Review through an isolated child session.
tools: read_file, grep, mcp__docs__*
user-invocable: false
disable-model-invocation: true
paths:
  - crates/**
---

# Reviewer
"#,
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");

    assert!(report.warnings.is_empty());
    let reviewer = descriptor(&report, "reviewer");
    assert_eq!(reviewer.run_as, SkillRunMode::ChildSession);
    assert_eq!(
        reviewer.description,
        "Review through an isolated child session."
    );
    assert!(!reviewer.model_invocable);
    assert!(!reviewer.user_invocable);
    assert!(reviewer.allowed_tools.names.contains("read_file"));
    assert!(reviewer.allowed_tools.names.contains("grep"));
    assert_eq!(reviewer.allowed_tools.prefixes, vec!["mcp__docs__"]);
    assert_eq!(reviewer.path_patterns, vec!["crates/**"]);
    assert_eq!(reviewer.entrypoint, Path::new(".claude/agents/reviewer.md"));
}

#[test]
fn compatibility_sources_control_foreign_skill_discovery() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".claude/skills/reviewer/SKILL.md"),
        r#"---
name: reviewer
description: Compatibility skill.
---

# Reviewer
"#,
    );
    let disabled_config = SkillConfig {
        compatibility_sources: Vec::new(),
        ..SkillConfig::default()
    };

    let disabled =
        discover_skill_index(workspace.path(), &disabled_config).expect("discovery should succeed");
    assert!(disabled.snapshot.descriptors.is_empty());

    let enabled = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    assert_eq!(
        descriptor(&enabled, "reviewer").description,
        "Compatibility skill."
    );
}

#[test]
fn invalid_paths_and_names_are_rejected_without_breaking_discovery() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let outside = tempfile::tempdir().expect("outside should create");
    write_skill(
        outside.path().join("skills/escape/SKILL.md"),
        r#"---
name: escape
description: Outside workspace.
---

# Escape
"#,
    );
    let escaping_config = SkillConfig {
        workspace_dir: outside.path().join("skills").display().to_string(),
        ..SkillConfig::default()
    };

    let escaping = discover_skill_index(workspace.path(), &escaping_config)
        .expect("discovery should succeed with warnings");

    assert!(escaping.snapshot.descriptors.is_empty());
    assert!(
        escaping
            .warnings
            .iter()
            .any(|warning| warning.kind == SkillDiscoveryWarningKind::InvalidPath)
    );

    write_skill(
        workspace.path().join(".sigil/skills/bad name/SKILL.md"),
        r#"---
description: Bad directory name.
---

# Bad
"#,
    );

    let invalid_name = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed with warnings");

    assert!(invalid_name.snapshot.descriptors.is_empty());
    assert!(
        invalid_name
            .warnings
            .iter()
            .any(|warning| warning.kind == SkillDiscoveryWarningKind::InvalidName)
    );
}

#[test]
fn malformed_list_frontmatter_reports_warning_without_panicking() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/bad-list/SKILL.md"),
        r#"---
name: bad-list
description: Bad list.
allowed-tools:
  read_file
---

# Bad List
"#,
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed with warnings");

    assert!(report.snapshot.descriptors.is_empty());
    let warning = report
        .warnings
        .iter()
        .find(|warning| warning.kind == SkillDiscoveryWarningKind::InvalidFrontmatter)
        .expect("invalid frontmatter warning should be present");
    assert!(warning.message.contains("unsupported list item"));
}

#[test]
fn skill_hash_uses_entrypoint_bytes_and_changes_with_body() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let path = workspace.path().join(".sigil/skills/hash/SKILL.md");
    let first = r#"---
name: hash
description: Hash test.
---

# Hash
"#;
    write_skill(&path, first);

    let first_report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    let first_descriptor = descriptor(&first_report, "hash");
    assert_eq!(
        first_descriptor.sha256,
        format!("{:x}", Sha256::digest(first.as_bytes()))
    );

    let second = r#"---
name: hash
description: Hash test.
---

# Hash

Changed body.
"#;
    write_skill(&path, second);
    let second_report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    let second_descriptor = descriptor(&second_report, "hash");
    assert_eq!(
        second_descriptor.sha256,
        format!("{:x}", Sha256::digest(second.as_bytes()))
    );
    assert_ne!(first_descriptor.sha256, second_descriptor.sha256);
}

#[test]
fn frontmatter_fields_project_to_skill_descriptor() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/repo-review/SKILL.md"),
        r#"---
id: repo-review
name: repo-review
description: "Review code # using repository standards."
when-to-use: Use before committing risky code.
run-as: child-session
agent: reviewer
trust: trusted
enabled: false
user-invocable: true
disable-model-invocation: false
argument-hint: scope
allowed-tools:
  - read_file
  - grep
disallowed-tools: [bash]
paths: [crates/**, dev/**]
---

# Repo Review
"#,
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");

    let descriptor = descriptor(&report, "repo-review");
    assert_eq!(
        descriptor.description,
        "Review code # using repository standards."
    );
    assert_eq!(
        descriptor.when_to_use.as_deref(),
        Some("Use before committing risky code.")
    );
    assert_eq!(descriptor.run_as, SkillRunMode::ChildSession);
    assert_eq!(descriptor.agent.as_deref(), Some("reviewer"));
    assert_eq!(descriptor.trust, SkillTrustState::Trusted);
    assert!(!descriptor.enabled);
    assert!(descriptor.user_invocable);
    assert!(descriptor.model_invocable);
    assert_eq!(descriptor.argument_hint.as_deref(), Some("scope"));
    assert!(descriptor.allowed_tools.names.contains("read_file"));
    assert!(descriptor.allowed_tools.names.contains("grep"));
    assert!(descriptor.disallowed_tools.names.contains("bash"));
    assert_eq!(descriptor.path_patterns, vec!["crates/**", "dev/**"]);
}

#[test]
fn plugin_skill_namespace_rejects_invalid_segments() {
    assert_eq!(
        namespaced_plugin_skill_id("review-pack", "repo-review").expect("id should build"),
        "review-pack/repo-review"
    );
    assert!(namespaced_plugin_skill_id("bad plugin", "repo-review").is_err());
    assert!(namespaced_plugin_skill_id("review-pack", "bad/skill").is_err());
}

#[test]
fn load_skill_model_index_truncates_and_preserves_absolute_paths() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    for index in 0..80 {
        let skill_id = format!("skill-{index:03}");
        write_skill(
            workspace
                .path()
                .join(".sigil/skills")
                .join(&skill_id)
                .join("SKILL.md"),
            &format!(
                r#"---
id: {skill_id}
description: "{}"
when-to-use: "{}"
trust: trusted
---

# {skill_id}
"#,
                "long description ".repeat(10),
                "long usage hint ".repeat(8)
            ),
        );
    }

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    let description = super::model_visible_skill_index_description(&report.snapshot);
    assert!(description.contains("\n- ..."));

    let absolute = workspace.path().join(".sigil/skills/skill-000/SKILL.md");
    assert_eq!(
        super::resolved_descriptor_path(workspace.path(), &absolute),
        absolute
    );
}

#[tokio::test]
async fn load_skill_tool_filters_model_index_and_loads_trusted_body() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let trusted_body = r#"---
name: repo-review
description: Review repositories.
when-to-use: Before risky commits.
trust: trusted
---

# Repo Review

Trusted Body Secret
"#;
    write_skill(
        workspace.path().join(".sigil/skills/repo-review/SKILL.md"),
        trusted_body,
    );
    write_skill(
        workspace.path().join(".sigil/skills/manual-only/SKILL.md"),
        r#"---
name: manual-only
description: Manual workflow.
trust: trusted
disable-model-invocation: true
---

# Manual
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/needs-review/SKILL.md"),
        r#"---
name: needs-review
description: Needs review.
---

# Needs Review
"#,
    );

    let mut registry = ToolRegistry::new();
    let report = register_skill_tools(
        &mut registry,
        workspace.path(),
        None,
        &SkillConfig::default(),
    )
    .expect("skill tools should register");
    assert_eq!(report.snapshot.descriptors.len(), 3);

    let spec = registry
        .spec_for(LOAD_SKILL_TOOL_NAME)
        .expect("load_skill spec should register");
    assert!(spec.description.contains("repo-review"));
    assert!(spec.description.contains("Review repositories."));
    assert!(!spec.description.contains("manual-only"));
    assert!(!spec.description.contains("needs-review"));

    let result = registry
        .execute(
            ToolContext {
                workspace_root: workspace.path().to_path_buf(),
                timeout_secs: 5,
            },
            tool_call("call-load-1", r#"{"id":"repo-review"}"#),
        )
        .await
        .expect("load_skill should execute");

    assert!(!result.is_error());
    assert_eq!(result.transient_context.len(), 1);
    let context = &result.transient_context[0];
    assert_eq!(context.role, MessageRole::System);
    let context_text = context
        .content
        .as_deref()
        .expect("context should have text");
    assert!(context_text.contains("id: repo-review"));
    assert!(context_text.contains("Trusted Body Secret"));
    assert!(!result.to_model_content().contains("Trusted Body Secret"));
    assert!(result.control_entries.iter().any(|entry| {
        matches!(
            entry,
            ControlEntry::SkillLoaded(loaded)
                if loaded.skill_id == "repo-review"
                    && loaded.call_id.as_deref() == Some("call-load-1")
                    && loaded.byte_count == trusted_body.len() as u64
        )
    }));

    let denied = registry
        .execute(
            ToolContext {
                workspace_root: workspace.path().to_path_buf(),
                timeout_secs: 5,
            },
            tool_call("call-load-2", r#"{"id":"manual-only"}"#),
        )
        .await
        .expect("load_skill should return structured errors");
    assert!(matches!(
        denied.status,
        ToolResultStatus::Error(ref error)
            if error.kind == ToolErrorKind::PermissionDenied
                && error.message.contains("not model-invocable")
    ));
}

#[test]
fn load_user_invoked_skill_allows_manual_only_skill() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/manual-only/SKILL.md"),
        r#"---
name: manual-only
description: Manual workflow.
trust: trusted
disable-model-invocation: true
user-invocable: true
---

# Manual

Manual Body Secret
"#,
    );
    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");

    let loaded = super::load_user_invoked_skill(
        workspace.path(),
        &report.snapshot,
        "manual-only",
        Some("run-7".to_owned()),
    )
    .expect("manual invocation should load a user-invocable skill");

    assert_eq!(loaded.descriptor.id, "manual-only");
    assert_eq!(loaded.entry.skill_id, "manual-only");
    assert_eq!(loaded.entry.run_id.as_deref(), Some("run-7"));
    assert!(loaded.entry.call_id.is_none());
    assert_eq!(loaded.transient_context.role, MessageRole::System);
    assert!(
        loaded
            .transient_context
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Manual Body Secret"))
    );
}

#[test]
fn load_user_invoked_skill_rejects_permission_and_identity_edges() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/manual-only/SKILL.md"),
        r#"---
name: manual-only
description: Manual workflow.
trust: trusted
disable-model-invocation: true
user-invocable: true
---

# Manual
"#,
    );
    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    let base = descriptor(&report, "manual-only").clone();

    let unknown =
        super::load_user_invoked_skill(workspace.path(), &report.snapshot, "missing", None)
            .expect_err("unknown skill should fail");
    assert!(unknown.to_string().contains("unknown skill"));

    let mut disabled = base.clone();
    disabled.enabled = false;
    assert_user_load_error(workspace.path(), disabled, "disabled");

    let mut hidden = base;
    hidden.user_invocable = false;
    assert_user_load_error(workspace.path(), hidden, "not user-invocable");
}

#[test]
fn load_user_invoked_skill_rejects_filesystem_and_body_edges() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/base/SKILL.md"),
        r#"---
name: base
description: Base workflow.
trust: trusted
user-invocable: true
---

# Base
"#,
    );
    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    let base = descriptor(&report, "base").clone();

    let mut missing_root = base.clone();
    missing_root.root = ".sigil/skills/missing-root".into();
    assert_user_load_error(
        workspace.path(),
        missing_root,
        "skill root cannot be resolved",
    );

    let mut missing_entrypoint = base.clone();
    missing_entrypoint.entrypoint = ".sigil/skills/base/MISSING.md".into();
    assert_user_load_error(
        workspace.path(),
        missing_entrypoint,
        "skill entrypoint cannot be resolved",
    );

    write_skill(
        workspace.path().join(".sigil/skills/other/SKILL.md"),
        r#"---
name: other
description: Other workflow.
trust: trusted
user-invocable: true
---

# Other
"#,
    );
    let mut escaped = base.clone();
    escaped.entrypoint = ".sigil/skills/other/SKILL.md".into();
    escaped.sha256 = format!(
        "{:x}",
        Sha256::digest(
            fs::read(workspace.path().join(".sigil/skills/other/SKILL.md"))
                .expect("other skill should read")
        )
    );
    assert_user_load_error(workspace.path(), escaped, "outside its skill root");

    let directory_entrypoint = workspace
        .path()
        .join(".sigil/skills/base/directory-entrypoint");
    fs::create_dir_all(&directory_entrypoint).expect("directory entrypoint should create");
    let mut directory = base.clone();
    directory.entrypoint = ".sigil/skills/base/directory-entrypoint".into();
    directory.sha256.clear();
    assert_user_load_error(workspace.path(), directory, "failed to read skill");

    let huge_path = workspace.path().join(".sigil/skills/base/huge.md");
    fs::write(&huge_path, vec![b'a'; 256 * 1024 + 1]).expect("huge skill should write");
    let mut huge = base.clone();
    huge.entrypoint = ".sigil/skills/base/huge.md".into();
    huge.sha256.clear();
    assert_user_load_error(workspace.path(), huge, "body is too large");

    let invalid_utf8_path = workspace.path().join(".sigil/skills/base/invalid.md");
    fs::write(&invalid_utf8_path, [0xff, 0xfe, 0xfd]).expect("invalid utf8 should write");
    let mut invalid_utf8 = base.clone();
    invalid_utf8.entrypoint = ".sigil/skills/base/invalid.md".into();
    invalid_utf8.sha256.clear();
    assert_user_load_error(workspace.path(), invalid_utf8, "is not utf-8");

    let many_lines_path = workspace.path().join(".sigil/skills/base/many-lines.md");
    fs::write(&many_lines_path, "x\n".repeat(8_001)).expect("many lines should write");
    let mut many_lines = base.clone();
    many_lines.entrypoint = ".sigil/skills/base/many-lines.md".into();
    many_lines.sha256.clear();
    assert_user_load_error(workspace.path(), many_lines, "too many lines");

    let mut hash_changed = base;
    hash_changed.sha256 = "not-the-current-hash".to_owned();
    assert_user_load_error(workspace.path(), hash_changed, "hash changed");
}

fn assert_user_load_error(
    workspace_root: &Path,
    descriptor: SkillDescriptor,
    expected_message: &str,
) {
    let skill_id = descriptor.id.clone();
    let snapshot = SkillIndexSnapshot::new(vec![descriptor]).expect("snapshot should build");
    let error = super::load_user_invoked_skill(workspace_root, &snapshot, &skill_id, None)
        .expect_err("user-invoked skill should fail");
    assert!(
        error.to_string().contains(expected_message),
        "expected {expected_message:?} in {error:#}"
    );
}

#[tokio::test]
async fn load_skill_tool_rejects_untrusted_and_root_escape_entries() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/needs-review/SKILL.md"),
        r#"---
name: needs-review
description: Needs review.
---

# Needs Review
"#,
    );
    let mut registry = ToolRegistry::new();
    register_skill_tools(
        &mut registry,
        workspace.path(),
        None,
        &SkillConfig::default(),
    )
    .expect("skill tools should register");

    let untrusted = registry
        .execute(
            ToolContext {
                workspace_root: workspace.path().to_path_buf(),
                timeout_secs: 5,
            },
            tool_call("call-load-untrusted", r#"{"id":"needs-review"}"#),
        )
        .await
        .expect("load_skill should return structured errors");
    assert!(matches!(
        untrusted.status,
        ToolResultStatus::Error(ref error)
            if error.kind == ToolErrorKind::PermissionDenied
                && error.message.contains("not trusted")
    ));

    write_skill(
        workspace.path().join(".sigil/skills/safe/SKILL.md"),
        r#"---
name: safe
trust: trusted
---

# Safe
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/other/SKILL.md"),
        r#"---
name: other
trust: trusted
---

# Other
"#,
    );
    let discovered = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");
    let mut escaped = descriptor(&discovered, "safe").clone();
    escaped.entrypoint = Path::new(".sigil/skills/other/SKILL.md").to_path_buf();
    escaped.sha256 = format!(
        "{:x}",
        Sha256::digest(
            fs::read(workspace.path().join(".sigil/skills/other/SKILL.md"))
                .expect("other skill should read")
        )
    );
    let tool = super::LoadSkillTool::new(
        workspace.path().to_path_buf(),
        SkillIndexSnapshot::new(vec![escaped]).expect("snapshot should build"),
    );

    let escaped_result = tool
        .execute(
            ToolContext {
                workspace_root: workspace.path().to_path_buf(),
                timeout_secs: 5,
            },
            "call-load-escape".to_owned(),
            serde_json::from_str(r#"{"id":"safe"}"#).expect("args should parse"),
        )
        .await
        .expect("load_skill should return structured errors");
    assert!(matches!(
        escaped_result.status,
        ToolResultStatus::Error(ref error)
            if error.kind == ToolErrorKind::PathOutsideWorkspace
                && error.message.contains("outside its skill root")
    ));
}

#[tokio::test]
async fn load_skill_agent_permission_modes_allow_ask_and_deny() -> Result<()> {
    let allow_workspace = tempfile::tempdir()?;
    write_load_skill_fixture(allow_workspace.path());
    let mut allow_approval = AutoApproveHandler;
    let (allow_output, allow_session, allow_requests) = run_load_skill_agent(
        allow_workspace.path(),
        PermissionConfig {
            tools: BTreeMap::from([(LOAD_SKILL_TOOL_NAME.to_owned(), ApprovalMode::Allow)]),
            ..PermissionConfig::default()
        },
        &mut allow_approval,
    )
    .await?;

    assert_eq!(allow_output.result.final_text, "done");
    assert!(allow_output.outcome.tool_errors.is_empty());
    assert_skill_loaded(&allow_session);
    assert_request_has_loaded_skill_body(&allow_requests);
    assert_approval_audit(
        &allow_session,
        ToolApprovalAuditAction::PolicyEvaluated,
        ApprovalMode::Allow,
        None,
    );

    let ask_workspace = tempfile::tempdir()?;
    write_load_skill_fixture(ask_workspace.path());
    let mut ask_approval = AutoApproveHandler;
    let (ask_output, ask_session, ask_requests) = run_load_skill_agent(
        ask_workspace.path(),
        PermissionConfig {
            tools: BTreeMap::from([(LOAD_SKILL_TOOL_NAME.to_owned(), ApprovalMode::Ask)]),
            ..PermissionConfig::default()
        },
        &mut ask_approval,
    )
    .await?;

    assert_eq!(ask_output.result.final_text, "done");
    assert!(ask_output.outcome.tool_errors.is_empty());
    assert_skill_loaded(&ask_session);
    assert_request_has_loaded_skill_body(&ask_requests);
    assert_approval_audit(
        &ask_session,
        ToolApprovalAuditAction::Requested,
        ApprovalMode::Ask,
        None,
    );
    assert_approval_audit(
        &ask_session,
        ToolApprovalAuditAction::Resolved,
        ApprovalMode::Ask,
        Some(ToolApprovalUserDecision::Approved),
    );

    let deny_workspace = tempfile::tempdir()?;
    write_load_skill_fixture(deny_workspace.path());
    let mut deny_approval = AutoApproveHandler;
    let (deny_output, deny_session, deny_requests) = run_load_skill_agent(
        deny_workspace.path(),
        PermissionConfig {
            tools: BTreeMap::from([(LOAD_SKILL_TOOL_NAME.to_owned(), ApprovalMode::Deny)]),
            ..PermissionConfig::default()
        },
        &mut deny_approval,
    )
    .await?;

    assert_eq!(deny_output.result.final_text, "done");
    assert!(deny_output.outcome.tool_errors.iter().any(|error| {
        error.kind == ToolErrorKind::PermissionDenied
            && error.message.contains("denied by permission policy")
    }));
    assert_no_skill_loaded(&deny_session);
    assert_request_omits_loaded_skill_body(&deny_requests);
    assert_approval_audit(
        &deny_session,
        ToolApprovalAuditAction::Resolved,
        ApprovalMode::Deny,
        Some(ToolApprovalUserDecision::Denied),
    );

    Ok(())
}

#[test]
fn disabled_config_skips_all_discovery_sources() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let user_config = tempfile::tempdir().expect("user config should create");
    write_skill(
        workspace.path().join(".sigil/skills/workspace/SKILL.md"),
        "# Workspace",
    );
    write_skill(user_config.path().join("skills/user/SKILL.md"), "# User");
    let config = SkillConfig {
        enabled: false,
        ..SkillConfig::default()
    };

    let report =
        discover_skill_index_with_user_dir(workspace.path(), Some(user_config.path()), &config)
            .expect("discovery should succeed");

    assert!(report.snapshot.descriptors.is_empty());
    assert!(report.warnings.is_empty());
}

#[test]
fn user_skill_and_agent_sources_obey_config_flags() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let user_config = tempfile::tempdir().expect("user config should create");
    write_skill(
        user_config.path().join("skills/user-skill/SKILL.md"),
        r#"---
description: User skill.
---

# User Skill
"#,
    );
    write_skill(
        user_config.path().join("agents/user-agent.md"),
        r#"---
description: User agent.
---

# User Agent
"#,
    );
    let only_agents = SkillConfig {
        user_skills: false,
        compatibility_sources: Vec::new(),
        ..SkillConfig::default()
    };

    let report = discover_skill_index_with_user_dir(
        workspace.path(),
        Some(user_config.path()),
        &only_agents,
    )
    .expect("discovery should succeed");

    assert_eq!(
        report
            .snapshot
            .descriptors
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>(),
        vec!["user-agent"]
    );
    let agent = descriptor(&report, "user-agent");
    assert_eq!(agent.source, SkillSource::User);
    assert_eq!(agent.run_as, SkillRunMode::ChildSession);
    assert!(agent.root.is_absolute());
    assert!(agent.entrypoint.is_absolute());

    let only_skills = SkillConfig {
        user_agents: false,
        compatibility_sources: Vec::new(),
        ..SkillConfig::default()
    };

    let report = discover_skill_index_with_user_dir(
        workspace.path(),
        Some(user_config.path()),
        &only_skills,
    )
    .expect("discovery should succeed");

    assert_eq!(
        report
            .snapshot
            .descriptors
            .iter()
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>(),
        vec!["user-skill"]
    );
    let skill = descriptor(&report, "user-skill");
    assert_eq!(skill.source, SkillSource::User);
    assert_eq!(skill.run_as, SkillRunMode::Inline);
}

#[test]
fn directory_noise_missing_entrypoints_and_invalid_agents_are_non_fatal() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/README.md"),
        "# Not a skill",
    );
    fs::create_dir_all(workspace.path().join(".sigil/skills/missing"))
        .expect("missing skill dir should create");
    write_skill(
        workspace.path().join(".sigil/agents/readme.txt"),
        "# Not an agent",
    );
    write_skill(
        workspace.path().join(".sigil/agents/bad name.md"),
        "# Bad Agent",
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed with warnings");

    assert!(report.snapshot.descriptors.is_empty());
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::InvalidPath
        && warning.message.contains("missing SKILL.md")));
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::InvalidName
        && warning.message.contains("invalid agent file name")));
}

#[test]
fn invalid_entrypoint_content_and_frontmatter_are_reported() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_bytes(
        workspace.path().join(".sigil/skills/binary/SKILL.md"),
        &[0xff, 0xfe, 0xfd],
    );
    write_skill(
        workspace.path().join(".sigil/skills/unterminated/SKILL.md"),
        r#"---
description: Missing close
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/no-colon/SKILL.md"),
        r#"---
description
---

# No Colon
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/empty-key/SKILL.md"),
        r#"---
: value
---

# Empty Key
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/multiline/SKILL.md"),
        r#"---
description: >
---

# Multiline
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/bad-id/SKILL.md"),
        r#"---
id: bad/id
---

# Bad Id
"#,
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed with warnings");

    assert!(report.snapshot.descriptors.is_empty());
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::ReadFailed
        && warning.message.contains("not utf-8")));
    assert!(
        report
            .warnings
            .iter()
            .filter(|warning| warning.kind == SkillDiscoveryWarningKind::InvalidFrontmatter)
            .count()
            >= 4
    );
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::InvalidName
        && warning.message.contains("invalid skill id")));
}

#[test]
fn invalid_descriptor_fields_are_frontmatter_warnings() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/skills/bad-bool/SKILL.md"),
        r#"---
disable-model-invocation: maybe
---

# Bad Bool
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/bad-run/SKILL.md"),
        r#"---
run-as: detached
---

# Bad Run
"#,
    );
    write_skill(
        workspace.path().join(".sigil/skills/bad-trust/SKILL.md"),
        r#"---
trust: permanent
---

# Bad Trust
"#,
    );
    write_skill(
        workspace
            .path()
            .join(".sigil/skills/bad-list-inline/SKILL.md"),
        r#"---
allowed-tools: [read_file
---

# Bad List
"#,
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed with warnings");

    assert!(report.snapshot.descriptors.is_empty());
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|warning| warning.kind == SkillDiscoveryWarningKind::InvalidFrontmatter)
            .count(),
        4
    );
}

#[test]
fn empty_entrypoint_and_tool_scope_edges_are_discovered() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(workspace.path().join(".sigil/skills/empty/SKILL.md"), "");
    write_skill(
        workspace.path().join(".sigil/skills/tool-scope/SKILL.md"),
        r#"---
description: Tool scope.
tools: all, , mcp__docs__*
disallowed-tools:
---

# Tool Scope
"#,
    );

    let report = discover_skill_index(workspace.path(), &SkillConfig::default())
        .expect("discovery should succeed");

    let empty = descriptor(&report, "empty");
    assert_eq!(empty.name, "empty");
    assert!(empty.description.is_empty());

    let tool_scope = descriptor(&report, "tool-scope");
    assert!(tool_scope.allowed_tools.allow_all);
    assert_eq!(tool_scope.allowed_tools.prefixes, vec!["mcp__docs__"]);
    assert!(tool_scope.allowed_tools.names.is_empty());
    assert!(tool_scope.disallowed_tools.names.is_empty());
}

#[test]
fn configured_file_paths_and_inline_parser_edges_are_reported() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    write_skill(
        workspace.path().join(".sigil/not-a-dir"),
        "# Not a directory",
    );
    write_skill(
        workspace.path().join(".sigil/skills/commented/SKILL.md"),
        r#"---
description: Review code # strips comment
allowed-tools: []
---

# Commented
"#,
    );
    write_skill(
        workspace
            .path()
            .join(".sigil/skills/bad-list-scalar/SKILL.md"),
        r#"---
paths:
  - >
---

# Bad List Scalar
"#,
    );
    let config = SkillConfig {
        workspace_agents_dir: ".sigil/not-a-dir".to_owned(),
        ..SkillConfig::default()
    };

    let report = discover_skill_index(workspace.path(), &config)
        .expect("discovery should succeed with warnings");

    let commented = descriptor(&report, "commented");
    assert_eq!(commented.description, "Review code");
    assert!(commented.allowed_tools.names.is_empty());
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::InvalidPath
        && warning.message.contains("not a directory")));
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::InvalidFrontmatter
        && warning.message.contains("unsupported multiline scalar")));
}

#[test]
fn private_parser_helpers_cover_scalar_and_id_edges() {
    assert_eq!(
        super::clean_scalar("'value # kept'").expect("quoted scalar should parse"),
        "value # kept"
    );
    assert_eq!(
        super::clean_scalar("value # dropped").expect("comment should strip"),
        "value"
    );
    assert!(super::clean_scalar(">").is_err());
    assert!(
        super::parse_inline_list("[]")
            .expect("empty list should parse")
            .is_empty()
    );
    assert!(super::parse_inline_list("[read_file").is_err());
    assert!(!super::valid_skill_id(""));
}

#[cfg(unix)]
#[test]
fn unix_filesystem_edges_are_non_fatal_warnings() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let outside = tempfile::tempdir().expect("outside should create");

    write_skill(outside.path().join("escape/SKILL.md"), "# Escape");
    fs::create_dir_all(workspace.path().join(".sigil/skills"))
        .expect("workspace skills dir should create");
    symlink(
        outside.path().join("escape"),
        workspace.path().join(".sigil/skills/escape-link"),
    )
    .expect("escape symlink should create");

    let unreadable_entrypoint = workspace.path().join(".sigil/skills/unreadable/SKILL.md");
    write_skill(&unreadable_entrypoint, "# Unreadable");
    let mut unreadable_permissions = fs::metadata(&unreadable_entrypoint)
        .expect("unreadable metadata should load")
        .permissions();
    unreadable_permissions.set_mode(0o000);
    fs::set_permissions(&unreadable_entrypoint, unreadable_permissions)
        .expect("unreadable permissions should set");

    let unreadable_agents_dir = workspace.path().join(".sigil/unreadable-agents");
    fs::create_dir_all(&unreadable_agents_dir).expect("unreadable agents dir should create");
    let mut unreadable_dir_permissions = fs::metadata(&unreadable_agents_dir)
        .expect("unreadable dir metadata should load")
        .permissions();
    unreadable_dir_permissions.set_mode(0o000);
    fs::set_permissions(&unreadable_agents_dir, unreadable_dir_permissions)
        .expect("unreadable dir permissions should set");

    let config = SkillConfig {
        workspace_agents_dir: ".sigil/unreadable-agents".to_owned(),
        ..SkillConfig::default()
    };
    let report = discover_skill_index(workspace.path(), &config)
        .expect("discovery should succeed with warnings");

    restore_permissions(&unreadable_entrypoint);
    restore_permissions(&unreadable_agents_dir);

    assert!(report.snapshot.descriptors.is_empty());
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == SkillDiscoveryWarningKind::InvalidPath
            && warning
                .message
                .contains("entrypoint escapes workspace root")
    }));
    assert!(report.warnings.iter().any(|warning| warning.kind
        == SkillDiscoveryWarningKind::ReadFailed
        && warning.message.contains("failed to read skill entrypoint")));
    assert!(report.warnings.iter().any(|warning| {
        warning.kind == SkillDiscoveryWarningKind::ReadFailed
            && warning
                .message
                .contains("failed to read skill discovery directory")
    }));
}

#[cfg(unix)]
fn restore_permissions(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("metadata should load for permission restore")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("permissions should restore");
}

async fn run_load_skill_agent<A>(
    workspace_root: &Path,
    permission_config: PermissionConfig,
    approval_handler: &mut A,
) -> Result<(AgentRunOutput, Session, Vec<CompletionRequest>)>
where
    A: ApprovalHandler + Send,
{
    let mut registry = ToolRegistry::new();
    register_skill_tools(&mut registry, workspace_root, None, &SkillConfig::default())?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        LoadSkillProvider {
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session = Session::new("mock-load-skill", "mock-model");
    let mut handler = NoopEventHandler;
    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("load repo review"),
            AgentRunOptions {
                workspace_root: workspace_root.to_path_buf(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config,
                permission_context: sigil_kernel::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            approval_handler,
        )
        .await?;
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned")
        .clone();
    Ok((output, session, requests))
}

fn write_load_skill_fixture(workspace_root: &Path) {
    write_skill(
        workspace_root.join(".sigil/skills/repo-review/SKILL.md"),
        r#"---
name: repo-review
description: Review repositories.
trust: trusted
---

# Repo Review

Agent Body Secret
"#,
    );
}

fn assert_skill_loaded(session: &Session) {
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SkillLoaded(loaded))
                if loaded.skill_id == "repo-review"
                    && loaded.call_id.as_deref() == Some("call-load-skill")
        )
    }));
}

fn assert_no_skill_loaded(session: &Session) {
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SkillLoaded(loaded))
                if loaded.skill_id == "repo-review"
        )
    }));
}

fn assert_request_has_loaded_skill_body(requests: &[CompletionRequest]) {
    assert!(
        requests
            .get(1)
            .expect("second request should include tool result context")
            .messages
            .iter()
            .any(|message| {
                message.role == MessageRole::System
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("Agent Body Secret"))
            })
    );
}

fn assert_request_omits_loaded_skill_body(requests: &[CompletionRequest]) {
    assert!(
        requests
            .iter()
            .flat_map(|request| request.messages.iter())
            .all(|message| !message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("Agent Body Secret")))
    );
}

fn assert_approval_audit(
    session: &Session,
    action: ToolApprovalAuditAction,
    policy_decision: ApprovalMode,
    user_decision: Option<ToolApprovalUserDecision>,
) {
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-load-skill"
                    && approval.tool_name == LOAD_SKILL_TOOL_NAME
                    && approval.action == action
                    && approval.policy_decision == policy_decision
                    && approval.user_decision == user_decision
        )
    }));
}

fn descriptor<'a>(
    report: &'a super::SkillDiscoveryReport,
    id: &str,
) -> &'a sigil_kernel::SkillDescriptor {
    report
        .snapshot
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id == id)
        .expect("descriptor should exist")
}

fn tool_call(id: &str, args_json: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: LOAD_SKILL_TOOL_NAME.to_owned(),
        args_json: args_json.to_owned(),
    }
}

fn write_skill(path: impl AsRef<Path>, content: &str) {
    write_bytes(path, content.as_bytes());
}

fn write_bytes(path: impl AsRef<Path>, content: &[u8]) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent should create");
    }
    fs::write(path, content).expect("skill should write");
}
