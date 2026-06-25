use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;
use sigil_kernel::{
    Agent, AgentConfig, AgentInvocationSource, AgentProfileId, AgentProfilePolicyEntry,
    AgentProfileTrustEntry, AgentRunInput, AgentRunOptions, AgentThreadStatus, AgentToolDelegate,
    AgentTrustState, ApprovalMode, AutoApproveHandler, CompactionConfig, CompletionRequest,
    ControlEntry, EventHandler, InteractionMode, JsonlSessionStore, MemoryConfig, MessageRole,
    PermissionAccessConfig, PermissionConfig, PermissionPolicy, PermissionPreset, Provider,
    ProviderCapabilities, ProviderChunk, ReasoningEffort, ReasoningStreamSupport, RootConfig,
    RunEvent, Session, SessionConfig, SessionLogEntry, ToolAccess, ToolCall, ToolCategory,
    ToolPreviewCapability, ToolRegistry, ToolSpec, ToolSubject, UsageStats, WorkspaceConfig,
};

use super::{
    AgentBudgetPolicy, AgentProfileRegistry, AgentSupervisor, AgentToolBackgroundRuns,
    AgentToolProviderFactory, AgentToolRuntime, CLOSE_AGENT_TOOL_NAME, MESSAGE_AGENT_TOOL_NAME,
    READ_AGENT_RESULT_TOOL_NAME, SPAWN_AGENT_TOOL_NAME, WAIT_AGENT_TOOL_NAME,
    chat_agent_thread_id_for_call, register_agent_tools,
    register_agent_tools_with_workspace_and_entries,
};

#[derive(Default)]
struct RecordingEventHandler {
    events: Vec<RunEvent>,
}

fn permission_test_spec(access: ToolAccess) -> ToolSpec {
    ToolSpec {
        name: "write_file".to_owned(),
        description: "write".to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        preview: ToolPreviewCapability::Required,
    }
}

#[test]
fn child_permission_config_keeps_parent_read_only_cap() -> Result<()> {
    let parent = PermissionConfig {
        preset: PermissionPreset::ReadOnly,
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
    let role = PermissionConfig {
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
    let profile = PermissionConfig::default();

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path("src/main.rs", "src/main.rs")],
    )?;

    assert_eq!(effective.preset, PermissionPreset::ReadOnly);
    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn child_permission_config_profile_deny_narrows_parent_allow() -> Result<()> {
    let parent = PermissionConfig {
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Allow),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
    let role = parent.clone();
    let profile = PermissionConfig {
        tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Deny)]),
        ..PermissionConfig::default()
    };

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path("src/main.rs", "src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn child_permission_config_profile_tool_allow_cannot_widen_parent_deny() -> Result<()> {
    let parent = PermissionConfig {
        access: PermissionAccessConfig {
            write: Some(ApprovalMode::Deny),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
    let role = PermissionConfig::default();
    let profile = PermissionConfig {
        tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path("src/main.rs", "src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn child_permission_config_profile_tool_allow_cannot_widen_parent_tool_deny() -> Result<()> {
    let parent = PermissionConfig {
        default_mode: ApprovalMode::Allow,
        tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Deny)]),
        ..PermissionConfig::default()
    };
    let role = PermissionConfig::default();
    let profile = PermissionConfig {
        tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Allow)]),
        ..PermissionConfig::default()
    };

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo test", "cargo test")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn child_permission_config_default_role_and_profile_inherit_parent_allow() -> Result<()> {
    let parent = PermissionConfig {
        default_mode: ApprovalMode::Allow,
        ..PermissionConfig::default()
    };
    let role = PermissionConfig::default();
    let profile = PermissionConfig::default();

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo test", "cargo test")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn child_permission_config_explicit_execute_ask_narrows_parent_allow() -> Result<()> {
    let parent = PermissionConfig {
        default_mode: ApprovalMode::Allow,
        ..PermissionConfig::default()
    };
    let role = PermissionConfig::default();
    let profile = PermissionConfig {
        access: PermissionAccessConfig {
            execute: Some(ApprovalMode::Ask),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Execute),
        "bash",
        vec![ToolSubject::command("cargo test", "cargo test")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn child_permission_config_profile_rule_allow_cannot_widen_parent_default_deny() -> Result<()> {
    let parent = PermissionConfig {
        default_mode: ApprovalMode::Deny,
        access: PermissionAccessConfig {
            read: Some(ApprovalMode::Deny),
            write: Some(ApprovalMode::Deny),
            execute: Some(ApprovalMode::Deny),
            network: Some(ApprovalMode::Deny),
        },
        ..PermissionConfig::default()
    };
    let role = PermissionConfig::default();
    let profile = PermissionConfig {
        rules: vec![sigil_kernel::PermissionRule {
            tool_name: Some("write_file".to_owned()),
            subject_glob: Some("src/**".to_owned()),
            mode: ApprovalMode::Allow,
        }],
        ..PermissionConfig::default()
    };

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Write),
        "write_file",
        vec![ToolSubject::path("src/main.rs", "src/main.rs")],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

#[test]
fn child_permission_config_external_rule_allow_cannot_widen_parent_default_deny() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_root = temp.path().canonicalize()?;
    let external_path = external_root.join("allowed").join("note.txt");
    std::fs::create_dir_all(external_path.parent().expect("path should have a parent"))?;
    std::fs::write(&external_path, "note")?;
    let parent = PermissionConfig {
        external_directory: sigil_kernel::ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Deny,
            ..sigil_kernel::ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    };
    let role = PermissionConfig {
        external_directory: sigil_kernel::ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Allow,
            ..sigil_kernel::ExternalDirectoryConfig::default()
        },
        ..PermissionConfig::default()
    };
    let profile = PermissionConfig {
        external_directory: sigil_kernel::ExternalDirectoryConfig {
            enabled: true,
            default_mode: ApprovalMode::Allow,
            rules: vec![sigil_kernel::ExternalDirectoryRule {
                path_glob: format!("{}/allowed/**", external_root.display()),
                mode: ApprovalMode::Allow,
            }],
        },
        ..PermissionConfig::default()
    };

    let effective = super::effective_child_permission_config(&parent, &role, &profile);
    let decision = PermissionPolicy::new(&effective).decide(
        &permission_test_spec(ToolAccess::Read),
        "read_file",
        vec![ToolSubject::path_with_scope(
            external_path.display().to_string(),
            external_path.display().to_string(),
            Some(external_path),
            sigil_kernel::ToolSubjectScope::External,
        )],
    )?;

    assert_eq!(decision.mode, ApprovalMode::Deny);
    Ok(())
}

impl EventHandler for RecordingEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }
}

fn assert_child_transcript_events_not_forwarded(handler: &RecordingEventHandler) {
    assert!(
        handler.events.iter().all(|event| {
            !matches!(
                event,
                RunEvent::TextDelta(_)
                    | RunEvent::ReasoningDelta(_)
                    | RunEvent::Usage(_)
                    | RunEvent::AssistantMessage(_)
                    | RunEvent::ToolCallStarted(_)
                    | RunEvent::ToolCallArgsDelta { .. }
                    | RunEvent::ToolCallCompleted(_)
                    | RunEvent::ToolResult(_)
                    | RunEvent::Notice(_)
                    | RunEvent::ContinuationState(_)
            )
        }),
        "child agent transcript/progress events must not be forwarded to the parent handler"
    );
}

fn assert_parent_agent_thread_controls_forwarded(handler: &RecordingEventHandler) {
    assert!(
        handler.events.iter().any(|event| {
            matches!(
                event,
                RunEvent::Control(ControlEntry::AgentThreadStarted(_))
            )
        }),
        "parent agent thread start control should still be forwarded"
    );
    assert!(
        handler.events.iter().any(|event| {
            matches!(
                event,
                RunEvent::Control(ControlEntry::AgentThreadResultRecorded(_))
            )
        }),
        "parent agent thread result control should still be forwarded"
    );
}

struct ChildTextProvider {
    text: String,
}

#[async_trait]
impl Provider for ChildTextProvider {
    fn name(&self) -> &str {
        "child-text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 3,
                completion_tokens: 2,
                ..UsageStats::default()
            })),
            Ok(ProviderChunk::TextDelta(self.text.clone())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ChildUsageProvider;

#[async_trait]
impl Provider for ChildUsageProvider {
    fn name(&self) -> &str {
        "child-usage"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 8,
                completion_tokens: 5,
                ..UsageStats::default()
            })),
            Ok(ProviderChunk::TextDelta("expensive child done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct DelayedFollowupProvider {
    delay: Duration,
    observed_followup: Arc<Mutex<bool>>,
}

#[async_trait]
impl Provider for DelayedFollowupProvider {
    fn name(&self) -> &str {
        "delayed-followup"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        tokio::time::sleep(self.delay).await;
        let followup_seen = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::User))
            .filter_map(|message| message.content.as_deref())
            .any(|content| content.contains("continue with more detail"));
        if followup_seen {
            *self
                .observed_followup
                .lock()
                .expect("followup observation lock should not be poisoned") = true;
        }
        let text = if followup_seen {
            "followup observed"
        } else {
            "initial background done"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(text.to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct RecordingChildProvider {
    observed_request: Arc<Mutex<Option<ChildRequestObservation>>>,
}

#[derive(Debug, Clone)]
struct ChildRequestObservation {
    system_messages: Vec<String>,
    user_messages: Vec<String>,
}

#[async_trait]
impl Provider for RecordingChildProvider {
    fn name(&self) -> &str {
        "recording-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let observation = ChildRequestObservation {
            system_messages: request
                .messages
                .iter()
                .filter(|message| matches!(message.role, MessageRole::System))
                .filter_map(|message| message.content.clone())
                .collect(),
            user_messages: request
                .messages
                .iter()
                .filter(|message| matches!(message.role, MessageRole::User))
                .filter_map(|message| message.content.clone())
                .collect(),
        };
        *self
            .observed_request
            .lock()
            .expect("child request observation lock should not be poisoned") = Some(observation);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("recorded child done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ParentSpawnProvider;

#[async_trait]
impl Provider for ParentSpawnProvider {
    fn name(&self) -> &str {
        "parent-spawn"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_result_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_result_seen {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "parent final includes child summary".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = json!({
            "profile_id": "explore",
            "objective": "inspect runtime",
            "prompt": "summarize runtime",
            "mode": "join_before_final",
            "display_name_hint": "runtime review"
        })
        .to_string();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-spawn-1".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: args,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ParentPreToolTextSpawnProvider;

#[async_trait]
impl Provider for ParentPreToolTextSpawnProvider {
    fn name(&self) -> &str {
        "parent-pre-tool-text-spawn"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_result_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_result_seen {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "parent final after child result".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = json!({
            "profile_id": "explore",
            "objective": "inspect kernel",
            "prompt": "summarize kernel",
            "mode": "join_before_final",
            "display_name_hint": "kernel review"
        })
        .to_string();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "parent pre-tool analysis that should not persist".to_owned(),
            )),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-spawn-pre-tool".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: args,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ParentReadAgentResultProvider {
    thread_id: sigil_kernel::AgentThreadId,
    page_text_marker: String,
    observed_second_request: Arc<Mutex<Option<ReadAgentResultRequestObservation>>>,
}

#[derive(Debug, Clone)]
struct ReadAgentResultRequestObservation {
    tool_message_contains_page_text: bool,
    transient_context_contains_page_text: bool,
}

#[async_trait]
impl Provider for ParentReadAgentResultProvider {
    fn name(&self) -> &str {
        "parent-read-agent-result"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_result_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_result_seen {
            let observation = ReadAgentResultRequestObservation {
                tool_message_contains_page_text: request.messages.iter().any(|message| {
                    matches!(message.role, MessageRole::Tool)
                        && message
                            .content
                            .as_deref()
                            .is_some_and(|content| content.contains(&self.page_text_marker))
                }),
                transient_context_contains_page_text: request.messages.iter().any(|message| {
                    matches!(message.role, MessageRole::User)
                        && message
                            .content
                            .as_deref()
                            .is_some_and(|content| content.contains(&self.page_text_marker))
                }),
            };
            *self
                .observed_second_request
                .lock()
                .expect("observation lock should not be poisoned") = Some(observation);
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "parent final after reading child page".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = json!({
            "thread_id": self.thread_id.as_str(),
            "offset_chars": 0,
            "max_chars": 4_000
        })
        .to_string();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-read-page".to_owned(),
                name: READ_AGENT_RESULT_TOOL_NAME.to_owned(),
                args_json: args,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct StaticProviderFactory;

impl AgentToolProviderFactory for StaticProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ChildTextProvider {
            text: "child summary only".to_owned(),
        }))
    }
}

struct RejectingProviderFactory;

impl AgentToolProviderFactory for RejectingProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        anyhow::bail!("provider factory should not be called for rejected profiles")
    }
}

struct TextProviderFactory {
    text: String,
}

impl AgentToolProviderFactory for TextProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ChildTextProvider {
            text: self.text.clone(),
        }))
    }
}

struct UsageProviderFactory;

impl AgentToolProviderFactory for UsageProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ChildUsageProvider))
    }
}

struct DelayedFollowupProviderFactory {
    observed_followup: Arc<Mutex<bool>>,
}

impl AgentToolProviderFactory for DelayedFollowupProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(DelayedFollowupProvider {
            delay: Duration::from_millis(20),
            observed_followup: self.observed_followup.clone(),
        }))
    }
}

struct RecordingProviderFactory {
    observed_request: Arc<Mutex<Option<ChildRequestObservation>>>,
}

impl AgentToolProviderFactory for RecordingProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(RecordingChildProvider {
            observed_request: self.observed_request.clone(),
        }))
    }
}

#[test]
fn spawn_agent_tool_schema_uses_stable_profile_id() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;

    let spec = registry
        .spec_for(SPAWN_AGENT_TOOL_NAME)
        .expect("spawn_agent registered");
    assert!(spec.description.contains("explore"));
    assert!(
        spec.description
            .contains("result_policy=summary_with_page_ref")
    );
    assert!(spec.description.contains("must delegate"));
    assert!(spec.description.contains("mode=background"));
    assert!(spec.description.contains("mode=join_before_final only"));
    assert!(!spec.description.contains("worker:"));
    assert!(spec.input_schema["properties"].get("profile_id").is_some());
    assert!(
        spec.input_schema["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|value| value == "profile_id"))
    );
    assert!(
        spec.input_schema["properties"]
            .get("display_name_hint")
            .is_some()
    );
    let wait_spec = registry
        .spec_for(WAIT_AGENT_TOOL_NAME)
        .expect("wait_agent registered");
    assert!(wait_spec.description.contains("short bounded interval"));
    assert!(
        wait_spec.input_schema["properties"]
            .get("result_offset_chars")
            .is_none()
    );
    assert!(
        wait_spec.input_schema["properties"]
            .get("result_max_chars")
            .is_none()
    );
    let read_spec = registry
        .spec_for(READ_AGENT_RESULT_TOOL_NAME)
        .expect("read_agent_result registered");
    assert!(
        read_spec.input_schema["properties"]
            .get("offset_chars")
            .is_some()
    );
    assert!(
        read_spec.input_schema["properties"]
            .get("max_chars")
            .is_some()
    );
    let modes = spec.input_schema["properties"]["mode"]["enum"]
        .as_array()
        .expect("mode enum");
    assert!(modes.iter().any(|mode| mode == "background"));
    assert_eq!(
        spec.input_schema["properties"]["mode"]["default"],
        "background"
    );
    assert!(registry.spec_for(MESSAGE_AGENT_TOOL_NAME).is_some());
    Ok(())
}

#[test]
fn agent_tool_permission_defaults_allow_safe_coordination_tools() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let ctx = sigil_kernel::ToolContext {
        workspace_root: std::env::temp_dir(),
        timeout_secs: 30,
    };
    let safe_spawn = ToolCall {
        id: "call-safe-spawn".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
        })
        .to_string(),
    };

    assert_eq!(
        registry.permission_default_mode(&ctx, &safe_spawn)?,
        Some(sigil_kernel::ApprovalMode::Allow)
    );

    for name in [
        WAIT_AGENT_TOOL_NAME,
        READ_AGENT_RESULT_TOOL_NAME,
        MESSAGE_AGENT_TOOL_NAME,
        CLOSE_AGENT_TOOL_NAME,
    ] {
        let call = ToolCall {
            id: format!("call-{name}"),
            name: name.to_owned(),
            args_json: json!({ "thread_id": "agent_chat_example" }).to_string(),
        };
        assert_eq!(
            registry.permission_default_mode(&ctx, &call)?,
            Some(sigil_kernel::ApprovalMode::Allow),
            "{name} should default to allow"
        );
    }
    Ok(())
}

#[test]
fn agent_tool_registration_uses_durable_profile_trust_projection() -> Result<()> {
    let config = root_config();
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("review");
    fs::create_dir_all(&agent_dir)?;
    let agent_file = agent_dir.join("agent.toml");
    fs::write(
        &agent_file,
        r#"
description = "Trusted review agent."
instructions = "Review the workspace."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let profile_id = AgentProfileId::new("review")?;
    let base_registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let snapshot = base_registry.capture_snapshot(&profile_id)?;
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::AgentProfileTrustDecision(AgentProfileTrustEntry {
            profile_id: profile_id.clone(),
            source: snapshot.source.clone(),
            source_hash: snapshot.source_hash.clone(),
            profile_hash: snapshot.profile_hash.clone(),
            decision: AgentTrustState::Trusted,
            reviewed_at_ms: 42,
        }),
    )];

    let mut trusted_tools = ToolRegistry::new();
    register_agent_tools_with_workspace_and_entries(
        &mut trusted_tools,
        &config,
        &workspace,
        &entries,
    )?;
    let trusted_spawn = trusted_tools
        .spec_for(SPAWN_AGENT_TOOL_NAME)
        .expect("spawn agent registered");
    assert!(trusted_spawn.description.contains("- review:"));

    fs::write(
        &agent_file,
        r#"
description = "Trusted review agent."
instructions = "Review the workspace and summarize risks."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let mut stale_tools = ToolRegistry::new();
    register_agent_tools_with_workspace_and_entries(
        &mut stale_tools,
        &config,
        &workspace,
        &entries,
    )?;
    let stale_spawn = stale_tools
        .spec_for(SPAWN_AGENT_TOOL_NAME)
        .expect("spawn agent registered");
    assert!(!stale_spawn.description.contains("- review:"));
    Ok(())
}

#[test]
fn agent_tool_registration_uses_durable_profile_policy_projection() -> Result<()> {
    let config = root_config();
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("review");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Trusted review agent."
instructions = "Review the workspace."
trust = "trusted"
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let profile_id = AgentProfileId::new("review")?;
    let base_registry = AgentProfileRegistry::from_root_config_with_workspace(&config, &workspace)?;
    let snapshot = base_registry.capture_snapshot(&profile_id)?;
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::AgentProfilePolicyDecision(AgentProfilePolicyEntry {
            profile_id,
            source: snapshot.source,
            source_hash: snapshot.source_hash,
            profile_hash: snapshot.profile_hash,
            enabled: None,
            user_invocable: None,
            model_invocable: Some(false),
            reviewed_at_ms: 42,
        }),
    )];

    let mut policy_tools = ToolRegistry::new();
    register_agent_tools_with_workspace_and_entries(
        &mut policy_tools,
        &config,
        &workspace,
        &entries,
    )?;
    let spawn = policy_tools
        .spec_for(SPAWN_AGENT_TOOL_NAME)
        .expect("spawn agent registered");
    assert!(!spawn.description.contains("- review:"));
    Ok(())
}

#[tokio::test]
async fn spawn_agent_injects_profile_prompt_into_child_request() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let observed_request = Arc::new(Mutex::new(None));
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(RecordingProviderFactory {
            observed_request: observed_request.clone(),
        }),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let call = ToolCall {
        id: "call-profile-prompt".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect runtime",
            "prompt": "summarize runtime",
            "mode": "join_before_final"
        })
        .to_string(),
    };

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &call,
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(!result.is_error());
    let observation = observed_request
        .lock()
        .expect("child request observation lock should not be poisoned")
        .clone()
        .expect("child provider saw a request");
    let profile_prompt = observation
        .system_messages
        .iter()
        .find(|message| message.contains("Agent profile: explore"))
        .expect("profile system prompt should be injected");
    assert!(
        profile_prompt.contains("Agent profile: explore"),
        "profile id should be injected into the child system prompt"
    );
    assert!(
        profile_prompt.contains("Read-only codebase exploration and verification agent."),
        "profile description should be injected into the child system prompt"
    );
    assert!(
        profile_prompt.contains("Inspect the repository with read-only tools"),
        "profile instructions should be injected into the child system prompt"
    );
    assert_eq!(observation.user_messages, vec!["summarize runtime"]);
    assert!(session.messages().iter().all(|message| {
        message
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("Agent profile: explore"))
    }));
    assert_child_transcript_events_not_forwarded(&handler);
    assert_parent_agent_thread_controls_forwarded(&handler);
    Ok(())
}

#[tokio::test]
async fn spawn_agent_preview_contains_source_trust_mode_scope_budget() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let preview = registry
        .preview(
            sigil_kernel::ToolContext {
                workspace_root: std::env::temp_dir(),
                timeout_secs: 30,
            },
            ToolCall {
                id: "call-preview".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
        )
        .await?
        .expect("spawn preview");

    assert!(preview.body.contains("source:"));
    assert!(preview.body.contains("trust:"));
    assert!(preview.body.contains("mode: join_before_final"));
    assert!(preview.body.contains("objective: inspect"));
    assert!(preview.body.contains("tool_scope:"));
    assert!(preview.body.contains("budget:"));
    Ok(())
}

#[tokio::test]
async fn ordinary_chat_explicit_subagent_prompt_spawns_child() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut agent_delegate = AgentToolRuntime::with_provider_factory(
        supervisor,
        config.clone(),
        registry.clone(),
        Arc::new(StaticProviderFactory),
    );
    let agent = Agent::new(ParentSpawnProvider, registry);
    let mut session = Session::new("parent-spawn", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("use a sub agent to inspect runtime"),
            run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
            &mut agent_delegate,
        )
        .await?;

    assert_eq!(
        output.result.final_text,
        "parent final includes child summary"
    );
    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("child agent projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(thread.display_name.as_deref(), Some("runtime review"));
    assert!(
        thread
            .result
            .as_ref()
            .is_some_and(|result| result.summary == "child summary only")
    );
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-spawn-1")
            && message.content.as_deref().is_some_and(|content| {
                content.contains("child summary only")
                    && content.contains(r#""display_name":"runtime review""#)
            })
    }));
    Ok(())
}

#[tokio::test]
async fn agent_tool_turn_does_not_persist_parent_pre_tool_text() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut agent_delegate = AgentToolRuntime::with_provider_factory(
        supervisor,
        config.clone(),
        registry.clone(),
        Arc::new(StaticProviderFactory),
    );
    let agent = Agent::new(ParentPreToolTextSpawnProvider, registry);
    let mut session = Session::new("parent-pre-tool-text-spawn", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("use a sub agent to inspect kernel"),
            run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
            &mut agent_delegate,
        )
        .await?;

    assert_eq!(output.result.final_text, "parent final after child result");
    assert!(
        handler.events.iter().any(|event| {
            matches!(event, RunEvent::TextDelta(text) if text.contains("parent pre-tool analysis"))
        }),
        "streaming text is still surfaced live"
    );
    assert!(
        session.messages().iter().all(|message| {
            message
                .content
                .as_deref()
                .is_none_or(|content| !content.contains("parent pre-tool analysis"))
        }),
        "agent tool preamble must not become replayed parent context"
    );
    Ok(())
}

#[tokio::test]
async fn wait_and_close_agent_use_bounded_thread_projection() -> Result<()> {
    let (mut runtime, mut session, thread_id) = spawned_runtime_session().await?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let options = run_options(std::env::temp_dir());

    let wait = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-wait".to_owned(),
                name: WAIT_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
            },
            &options,
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("wait_agent handled");
    let wait_payload: serde_json::Value = serde_json::from_str(&wait.content)?;
    assert_eq!(wait_payload["status"], "completed");
    assert_eq!(wait_payload["result_available"], true);
    assert_eq!(
        wait_payload["result_ref"]["read_tool"],
        READ_AGENT_RESULT_TOOL_NAME
    );
    assert!(wait_payload.get("summary").is_none());
    assert!(!wait.content.contains("child summary only"));
    assert!(!wait.content.contains("system:base"));

    let close = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-close".to_owned(),
                name: CLOSE_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
            },
            &options,
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("close_agent handled");
    assert!(!close.is_error());
    assert!(close.control_entries.iter().any(|entry| {
        matches!(entry, ControlEntry::AgentThreadClosed(close) if close.thread_id == thread_id)
    }));

    let direct_close = crate::close_agent_thread(
        &session,
        thread_id.clone(),
        Some("closed from TUI /agent".to_owned()),
    );
    assert!(!direct_close.is_error());
    assert!(direct_close.control_entries.iter().any(|entry| {
        matches!(
            entry,
            ControlEntry::AgentThreadClosed(close)
                if close.thread_id == thread_id
                    && close.reason.as_deref() == Some("closed from TUI /agent")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn message_agent_records_rejected_message_route_for_terminal_thread() -> Result<()> {
    let (mut runtime, mut session, thread_id) = spawned_runtime_session().await?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-message".to_owned(),
                name: MESSAGE_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "thread_id": thread_id.as_str(),
                    "prompt": "continue with more detail"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("message_agent handled by runtime delegate");

    assert!(result.is_error());
    assert!(result.content.contains("cannot accept safe-point messages"));
    let routed = result
        .control_entries
        .iter()
        .filter_map(|entry| {
            let ControlEntry::AgentThreadMessageRouted(route) = entry else {
                return None;
            };
            Some(route)
        })
        .collect::<Vec<_>>();
    assert_eq!(routed.len(), 2);
    assert_eq!(routed[0].source_thread_id.as_str(), "main");
    assert_eq!(routed[0].target_thread_id, thread_id);
    assert_eq!(routed[0].status, sigil_kernel::AgentRouteStatus::Requested);
    assert_eq!(
        routed[0].prompt.as_deref(),
        Some("continue with more detail")
    );
    assert_eq!(routed[1].route_id, routed[0].route_id);
    assert_eq!(routed[1].source_thread_id, routed[0].source_thread_id);
    assert_eq!(routed[1].target_thread_id, routed[0].target_thread_id);
    assert_eq!(routed[1].prompt_hash, routed[0].prompt_hash);
    assert_eq!(routed[1].prompt, None);
    assert_eq!(routed[1].status, sigil_kernel::AgentRouteStatus::Rejected);
    assert!(!routed[0].prompt_hash.contains("continue with more detail"));
    Ok(())
}

#[tokio::test]
async fn message_agent_queues_followup_for_background_mailbox() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let observed_followup = Arc::new(Mutex::new(false));
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(DelayedFollowupProviderFactory {
            observed_followup: observed_followup.clone(),
        }),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let thread_id =
        chat_agent_thread_id_for_call("call-background-message", &AgentProfileId::new("explore")?)?;

    let spawn = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-background-message".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "background"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    assert!(!spawn.is_error());

    let message = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-message-background".to_owned(),
                name: MESSAGE_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "thread_id": thread_id.as_str(),
                    "prompt": "continue with more detail"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("message handled");
    assert!(!message.is_error());
    let payload: serde_json::Value = serde_json::from_str(&message.content)?;
    assert_eq!(payload["delivery"], "delivered_to_mailbox");
    assert_eq!(payload["delivered_to_mailbox"], true);
    assert_eq!(payload["safe_point"], "after_current_turn");
    assert_eq!(payload["will_apply_after_current_turn"], true);
    assert_eq!(payload["interrupt_requested"], false);
    assert_eq!(payload["interrupts_in_flight_provider_stream"], false);
    assert!(
        payload["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("wait_agent"))
    );
    let routed = message
        .control_entries
        .iter()
        .filter_map(|entry| {
            let ControlEntry::AgentThreadMessageRouted(route) = entry else {
                return None;
            };
            Some(route)
        })
        .collect::<Vec<_>>();
    assert_eq!(routed.len(), 2);
    assert_eq!(routed[0].status, sigil_kernel::AgentRouteStatus::Requested);
    assert_eq!(
        routed[0].prompt.as_deref(),
        Some("continue with more detail")
    );
    assert_eq!(routed[1].status, sigil_kernel::AgentRouteStatus::Resolved);
    assert_eq!(routed[1].prompt, None);

    for _ in 0..20 {
        let _ = runtime
            .handle_agent_tool_call(
                &mut session,
                &ToolCall {
                    id: "call-wait-background-message".to_owned(),
                    name: WAIT_AGENT_TOOL_NAME.to_owned(),
                    args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
                },
                &run_options(std::env::temp_dir()),
                &mut handler,
                &mut approval,
            )
            .await?
            .expect("wait handled");
        if session
            .agent_thread_state_projection()
            .threads
            .get(&thread_id)
            .and_then(|thread| thread.result.as_ref())
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        *observed_followup
            .lock()
            .expect("followup observation lock should not be poisoned")
    );
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("background thread should be projected");
    assert_eq!(
        thread.result.as_ref().map(|result| result.summary.as_str()),
        Some("followup observed")
    );
    Ok(())
}

#[tokio::test]
async fn spawn_agent_background_mode_starts_running_thread() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-background".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "background"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(!result.is_error());
    assert!(result.content.contains("running"));
    let payload: serde_json::Value = serde_json::from_str(&result.content)?;
    assert_eq!(payload["retry_after_ms"], 5_000);
    assert_eq!(payload["next_poll_after_ms"], 5_000);
    assert!(
        payload["next_poll_after_unix_ms"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(
        payload["next_action"]
            .as_str()
            .is_some_and(|action| action.contains("do not call wait_agent again immediately"))
    );
    let projection = session.agent_thread_state_projection();
    let thread_id = chat_agent_thread_id_for_call(
        "call-background",
        &sigil_kernel::AgentProfileId::new("explore")?,
    )?;
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("background thread should be started");
    assert_eq!(thread.status, AgentThreadStatus::Running);
    Ok(())
}

#[tokio::test]
async fn join_before_final_agent_can_be_moved_to_background() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let request_supervisor = supervisor.clone();
    let observed_followup = Arc::new(Mutex::new(false));
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(DelayedFollowupProviderFactory { observed_followup }),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let thread_id =
        chat_agent_thread_id_for_call("call-join-background", &AgentProfileId::new("explore")?)?;
    let spawn_call = ToolCall {
        id: "call-join-background".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };

    let request_background = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        request_supervisor.request_foreground_background()
    };
    let options = run_options(std::env::temp_dir());
    let (spawn, requested_thread_id) = tokio::join!(
        runtime.handle_agent_tool_call(
            &mut session,
            &spawn_call,
            &options,
            &mut handler,
            &mut approval,
        ),
        request_background,
    );
    let requested_thread_id = requested_thread_id.map_err(|error| anyhow::anyhow!(error))?;
    assert_eq!(requested_thread_id, thread_id);
    let spawn = spawn?.expect("spawn handled");

    assert!(!spawn.is_error());
    assert_eq!(spawn.metadata.details["status"], "running");
    let payload: serde_json::Value = serde_json::from_str(&spawn.content)?;
    assert_eq!(payload["terminal"], false);
    assert_eq!(payload["result_available"], false);
    assert_eq!(payload["backgrounded"], true);
    assert_eq!(payload["do_not_describe_as_finished"], true);
    assert!(payload["next_action"].as_str().is_some_and(|action| {
        action.contains("continue independent parent work") && action.contains("wait_agent")
    }));
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("detached thread should be projected");
    assert_eq!(thread.status, AgentThreadStatus::Running);
    assert_eq!(thread.reason.as_deref(), Some("agent moved to background"));

    let mut collected = None;
    for _ in 0..20 {
        let wait = runtime
            .handle_agent_tool_call(
                &mut session,
                &ToolCall {
                    id: "call-wait-detached".to_owned(),
                    name: WAIT_AGENT_TOOL_NAME.to_owned(),
                    args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
                },
                &run_options(std::env::temp_dir()),
                &mut handler,
                &mut approval,
            )
            .await?
            .expect("wait handled");
        if session
            .agent_thread_state_projection()
            .threads
            .get(&thread_id)
            .and_then(|thread| thread.result.as_ref())
            .is_some()
        {
            collected = Some(wait);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let collected = collected.expect("detached agent result should be collected");
    assert!(!collected.is_error());
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("detached thread should stay projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(
        thread.result.as_ref().map(|result| result.summary.as_str()),
        Some("initial background done")
    );
    Ok(())
}

#[tokio::test]
async fn moved_to_background_agent_can_be_collected_by_later_runtime() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let request_supervisor = supervisor.clone();
    let background_runs = AgentToolBackgroundRuns::default();
    let observed_followup = Arc::new(Mutex::new(false));
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor.clone(),
        config.clone(),
        registry.clone(),
        Arc::new(DelayedFollowupProviderFactory { observed_followup }),
    )
    .with_background_runs(background_runs.clone());
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let thread_id =
        chat_agent_thread_id_for_call("call-shared-background", &AgentProfileId::new("explore")?)?;
    let spawn_call = ToolCall {
        id: "call-shared-background".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };

    let request_background = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        request_supervisor.request_foreground_background()
    };
    let options = run_options(std::env::temp_dir());
    let (spawn, requested_thread_id) = tokio::join!(
        runtime.handle_agent_tool_call(
            &mut session,
            &spawn_call,
            &options,
            &mut handler,
            &mut approval,
        ),
        request_background,
    );
    let requested_thread_id = requested_thread_id.map_err(anyhow::Error::msg)?;
    assert_eq!(requested_thread_id, thread_id);
    let spawn = spawn?.expect("spawn handled");
    assert!(!spawn.is_error());
    assert_eq!(spawn.metadata.details["status"], "running");
    drop(runtime);

    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(background_runs.has_finished());
    let mut collector = AgentToolRuntime::with_provider_factory(
        supervisor.clone(),
        config.clone(),
        registry.clone(),
        Arc::new(StaticProviderFactory),
    )
    .with_background_runs(background_runs);
    let collected = collector
        .collect_finished_background_runs(&mut session, &mut handler)
        .await?;
    assert_eq!(collected, vec![thread_id.clone()]);

    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("background thread should be projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(
        thread.result.as_ref().map(|result| result.summary.as_str()),
        Some("initial background done")
    );
    assert!(supervisor.active_profile_ids().is_empty());

    let second = collector
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-after-collect".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect again",
                    "prompt": "inspect again",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("second spawn handled");
    assert!(!second.is_error());
    assert!(!second.content.contains("max_parallel_readonly"));
    Ok(())
}

#[tokio::test]
async fn wait_agent_collects_completed_background_result() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let thread_id =
        chat_agent_thread_id_for_call("call-background-wait", &AgentProfileId::new("explore")?)?;

    let spawn = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-background-wait".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "background"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    assert!(!spawn.is_error());

    let mut waited = None;
    for _ in 0..10 {
        let result = runtime
            .handle_agent_tool_call(
                &mut session,
                &ToolCall {
                    id: "call-wait-background".to_owned(),
                    name: WAIT_AGENT_TOOL_NAME.to_owned(),
                    args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
                },
                &run_options(std::env::temp_dir()),
                &mut handler,
                &mut approval,
            )
            .await?
            .expect("wait handled");
        if session
            .agent_thread_state_projection()
            .threads
            .get(&thread_id)
            .and_then(|thread| thread.result.as_ref())
            .is_some()
        {
            waited = Some(result);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let waited = waited.expect("background result should be collected");
    assert!(!waited.is_error());
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("background thread should be projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(
        thread.result.as_ref().map(|result| result.summary.as_str()),
        Some("child summary only")
    );
    Ok(())
}

#[tokio::test]
async fn wait_agent_waits_briefly_for_running_background_result() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let observed_followup = Arc::new(Mutex::new(false));
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(DelayedFollowupProviderFactory { observed_followup }),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let thread_id = chat_agent_thread_id_for_call(
        "call-background-brief-wait",
        &AgentProfileId::new("explore")?,
    )?;

    let spawn = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-background-brief-wait".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "background"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    assert!(!spawn.is_error());

    let wait = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-background-brief-wait-result".to_owned(),
                name: WAIT_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("wait handled");

    assert!(!wait.is_error());
    let payload: serde_json::Value = serde_json::from_str(&wait.content)?;
    assert_eq!(payload["status"], "completed");
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("background thread should be projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(
        thread.result.as_ref().map(|result| result.summary.as_str()),
        Some("initial background done")
    );
    Ok(())
}

#[tokio::test]
async fn wait_agent_throttles_repeated_pending_status_for_same_thread() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::new(supervisor, config, registry);
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_pending")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore")?;

    session.append_control(ControlEntry::AgentProfileCaptured(
        sigil_kernel::AgentProfileCapturedEntry {
            snapshot: sigil_kernel::AgentProfileSnapshot {
                snapshot_id: snapshot_id.clone(),
                profile_id: profile_id.clone(),
                source: sigil_kernel::AgentProfileSource::System,
                source_hash: "sha256:source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                resolved_tool_scope_hash: "tools".to_owned(),
                resolved_permission_policy_hash: "permissions".to_owned(),
                resolved_mcp_scope_hash: "mcp".to_owned(),
                resolved_skill_hashes: Vec::new(),
                trust_state: sigil_kernel::AgentTrustState::Trusted,
            },
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStarted(
        sigil_kernel::AgentThreadStartedEntry {
            thread_id: thread_id.clone(),
            parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            thread_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/agents/agent_chat_pending.jsonl",
            )?,
            profile_id,
            profile_snapshot_id: snapshot_id.clone(),
            run_context: sigil_kernel::AgentRunContextSnapshot {
                profile_snapshot_id: snapshot_id,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                reasoning_effort: None,
                workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                effective_tool_scope_hash: "tools".to_owned(),
                effective_permission_policy_hash: "permissions".to_owned(),
                effective_mcp_scope_hash: "mcp".to_owned(),
                provider_capability_hash: "provider".to_owned(),
                model_visible_agent_index_hash: Some("agents".to_owned()),
                budget_policy_hash: "budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "inspect".to_owned(),
            prompt_hash: "sha256:prompt".to_owned(),
            invocation_mode: sigil_kernel::AgentInvocationMode::Background,
            invocation_source: sigil_kernel::AgentInvocationSource::Chat,
            display_name: Some("pending".to_owned()),
            created_at_ms: Some(1),
        },
    ))?;
    session.append_control(ControlEntry::AgentThreadStatusChanged(
        sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: AgentThreadStatus::Running,
            reason: Some("still running".to_owned()),
            updated_at_ms: Some(2),
        },
    ))?;

    let wait_call = |id: &str| ToolCall {
        id: id.to_owned(),
        name: WAIT_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
    };
    let first = runtime
        .handle_agent_tool_call(
            &mut session,
            &wait_call("call-wait-first"),
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("first wait handled");
    let second = runtime
        .handle_agent_tool_call(
            &mut session,
            &wait_call("call-wait-second"),
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("second wait handled");

    let first_payload: serde_json::Value = serde_json::from_str(&first.content)?;
    let second_payload: serde_json::Value = serde_json::from_str(&second.content)?;
    assert_eq!(first_payload["status"], "running");
    assert_eq!(first_payload["coalesced"], serde_json::Value::Null);
    assert_eq!(second_payload["status"], "running");
    assert_eq!(second_payload["coalesced"], true);
    assert_eq!(second_payload["polling_throttled"], true);
    assert_eq!(
        second_payload["coalescing_key"],
        "wait_agent:agent_chat_pending"
    );
    assert!(
        second_payload["retry_after_ms"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(
        second_payload["next_poll_after_ms"],
        second_payload["retry_after_ms"]
    );
    assert!(
        second_payload["next_poll_after_unix_ms"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    Ok(())
}

#[test]
fn wait_agent_throttle_expiry_does_not_panic_after_interval() {
    let expired_wait =
        Instant::now() - super::WAIT_AGENT_MIN_REPOLL_INTERVAL - Duration::from_millis(1);

    assert_eq!(super::wait_throttle_remaining_since(expired_wait), None);
}

#[tokio::test]
async fn spawn_agent_rejects_model_invisible_profile_before_building_provider() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(RejectingProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-model-invisible".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "plan",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(result.is_error());
    assert!(result.content.contains("not model-invocable"));
    assert!(session.agent_thread_state_projection().threads.is_empty());
    Ok(())
}

#[tokio::test]
async fn manual_agent_invocation_allows_user_invocable_model_hidden_profile() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let invocation = runtime
        .invoke_agent_profile(
            &mut session,
            AgentProfileId::new("plan")?,
            "draft an implementation plan".to_owned(),
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?;

    let result = invocation
        .result
        .as_ref()
        .expect("manual invocation should record terminal result");
    assert!(result.summary.contains("child summary only"));
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&invocation.thread_id)
        .expect("manual invocation should create an agent thread");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert_eq!(
        thread.invocation_source,
        Some(AgentInvocationSource::Mention)
    );
    assert_eq!(
        thread.profile_id.as_ref().map(AgentProfileId::as_str),
        Some("plan")
    );
    let mut second_session = Session::new("parent", "model");
    let second_invocation = runtime
        .invoke_agent_profile(
            &mut second_session,
            AgentProfileId::new("plan")?,
            "draft an implementation plan".to_owned(),
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?;
    assert_ne!(invocation.thread_id, second_invocation.thread_id);
    assert_child_transcript_events_not_forwarded(&handler);
    assert_parent_agent_thread_controls_forwarded(&handler);
    Ok(())
}

#[tokio::test]
async fn wait_agent_reports_status_without_repeating_bounded_summary() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry.clone(),
        Arc::new(TextProviderFactory {
            text: "x".repeat(5_001),
        }),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let call = ToolCall {
        id: "call-long".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };
    let _ = runtime
        .handle_agent_tool_call(
            &mut session,
            &call,
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let thread_id =
        chat_agent_thread_id_for_call(&call.id, &sigil_kernel::AgentProfileId::new("explore")?)?;
    let projection = session.agent_thread_state_projection();
    let result = projection
        .threads
        .get(&thread_id)
        .and_then(|thread| thread.result.as_ref())
        .expect("thread result");
    assert_eq!(result.summary.chars().count(), 4_000);
    assert!(result.summary_truncated);
    assert_eq!(result.original_summary_chars, Some(5_001));

    let wait = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-wait-long".to_owned(),
                name: WAIT_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "thread_id": thread_id.as_str()
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("wait handled");
    let payload: serde_json::Value = serde_json::from_str(&wait.content)?;
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["result_available"], true);
    assert_eq!(payload["result_ref"]["summary_truncated"], true);
    assert_eq!(payload["result_ref"]["original_summary_chars"], 5_001);
    assert_eq!(
        payload["result_ref"]["read_args"]["max_chars"],
        serde_json::Value::from(4_000)
    );
    assert!(payload.get("summary").is_none());
    assert!(!wait.content.contains(&"x".repeat(200)));
    Ok(())
}

#[tokio::test]
async fn read_agent_result_pages_full_child_result_from_child_session() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let full_text = format!("alpha\n{}\nomega", "x".repeat(5_200));
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(TextProviderFactory {
            text: full_text.clone(),
        }),
    );
    let temp = tempfile::tempdir()?;
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("parent", "model", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let spawn_call = ToolCall {
        id: "call-page".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };

    let spawn_result = runtime
        .handle_agent_tool_call(
            &mut session,
            &spawn_call,
            &run_options(temp.path().to_path_buf()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let spawn_payload: serde_json::Value = serde_json::from_str(&spawn_result.content)?;
    assert_eq!(spawn_payload["summary_truncated"], true);
    assert_eq!(spawn_payload["full_result_available"], true);
    assert_eq!(spawn_payload["artifacts"][0]["kind"], "child_session");
    assert_eq!(
        spawn_payload["result_fetch"]["tool"],
        READ_AGENT_RESULT_TOOL_NAME
    );
    let thread_id = chat_agent_thread_id_for_call(
        &spawn_call.id,
        &sigil_kernel::AgentProfileId::new("explore")?,
    )?;

    let read_result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-page-read".to_owned(),
                name: READ_AGENT_RESULT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "thread_id": thread_id.as_str(),
                    "offset_chars": 4_900,
                    "max_chars": 800
                })
                .to_string(),
            },
            &run_options(temp.path().to_path_buf()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("read handled");
    let read_payload: serde_json::Value = serde_json::from_str(&read_result.content)?;
    assert!(read_payload.get("summary").is_none());
    let page = &read_payload["page"];

    assert_eq!(page["offset_chars"], 4_900);
    assert_eq!(page["total_chars"], full_text.chars().count());
    assert!(page.get("text").is_none());
    assert_eq!(page["text_omitted"], true);
    assert_eq!(page["text_delivery"], "transient_context");
    assert_eq!(page["truncated"], false);
    assert!(page["next_offset_chars"].is_null());
    assert_eq!(read_result.transient_context.len(), 1);
    assert!(
        read_result.transient_context[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("omega"))
    );
    Ok(())
}

#[tokio::test]
async fn read_agent_result_failure_does_not_overwrite_completed_agent_status() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(TextProviderFactory {
            text: "child completed before page read failed".to_owned(),
        }),
    );
    let temp = tempfile::tempdir()?;
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("parent", "model", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let spawn_call = ToolCall {
        id: "call-read-failure-spawn".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };
    let _ = runtime
        .handle_agent_tool_call(
            &mut session,
            &spawn_call,
            &run_options(temp.path().to_path_buf()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let thread_id = chat_agent_thread_id_for_call(
        &spawn_call.id,
        &sigil_kernel::AgentProfileId::new("explore")?,
    )?;
    let child_path = {
        let projection = session.agent_thread_state_projection();
        let result = projection
            .threads
            .get(&thread_id)
            .and_then(|thread| thread.result.as_ref())
            .expect("completed child result");
        assert_eq!(
            result.status,
            sigil_kernel::AgentThreadTerminalStatus::Completed
        );
        let parent_dir = session
            .store_path()
            .and_then(std::path::Path::parent)
            .expect("parent session should have store parent");
        result.session_ref.resolve(parent_dir)
    };
    fs::remove_file(&child_path)?;

    let read_result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-page-read-missing-child".to_owned(),
                name: READ_AGENT_RESULT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "thread_id": thread_id.as_str(),
                    "offset_chars": 0,
                    "max_chars": 800
                })
                .to_string(),
            },
            &run_options(temp.path().to_path_buf()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("read handled as tool result");

    assert!(read_result.is_error());
    assert!(read_result.content.contains("child agent session"));
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("thread projection remains available");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert!(
        thread.result.as_ref().is_some_and(
            |result| result.status == sigil_kernel::AgentThreadTerminalStatus::Completed
        )
    );
    Ok(())
}

#[tokio::test]
async fn read_agent_result_page_text_is_transient_not_parent_tool_history() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let page_text_marker = "SECRET_CHILD_PAGE_MARKER";
    let full_text = format!("alpha {page_text_marker} omega");
    let mut agent_delegate = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry.clone(),
        Arc::new(TextProviderFactory {
            text: full_text.clone(),
        }),
    );
    let temp = tempfile::tempdir()?;
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("parent", "model", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let spawn_call = ToolCall {
        id: "call-read-transient-spawn".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };
    let _ = agent_delegate
        .handle_agent_tool_call(
            &mut session,
            &spawn_call,
            &run_options(temp.path().to_path_buf()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let thread_id = chat_agent_thread_id_for_call(
        &spawn_call.id,
        &sigil_kernel::AgentProfileId::new("explore")?,
    )?;
    let projection = session.agent_thread_state_projection();
    let child_result = projection
        .threads
        .get(&thread_id)
        .and_then(|thread| thread.result.as_ref())
        .expect("child result should be recorded");
    let final_answer_ref = child_result
        .final_answer_ref
        .as_ref()
        .expect("child result should record final answer ref");
    assert_eq!(final_answer_ref.session_ref, child_result.session_ref);
    assert_eq!(final_answer_ref.char_count, full_text.chars().count());
    let observed_second_request = Arc::new(Mutex::new(None));
    let agent = Agent::new(
        ParentReadAgentResultProvider {
            thread_id,
            page_text_marker: page_text_marker.to_owned(),
            observed_second_request: Arc::clone(&observed_second_request),
        },
        registry,
    );

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("read the child page"),
            run_options(temp.path().to_path_buf()),
            &mut handler,
            &mut approval,
            &mut agent_delegate,
        )
        .await?;

    assert_eq!(
        output.result.final_text,
        "parent final after reading child page"
    );
    let observation = observed_second_request
        .lock()
        .expect("observation lock should not be poisoned")
        .clone()
        .expect("second provider request should be observed");
    assert!(!observation.tool_message_contains_page_text);
    assert!(observation.transient_context_contains_page_text);
    let messages = session.messages();
    let read_tool_message = messages
        .iter()
        .find(|message| {
            matches!(message.role, MessageRole::Tool)
                && message.tool_call_id.as_deref() == Some("call-read-page")
        })
        .expect("read_agent_result tool message should persist metadata");
    let read_tool_content = read_tool_message
        .content
        .as_deref()
        .expect("tool message should have content");
    assert!(read_tool_content.contains("text_omitted"));
    assert!(read_tool_content.contains("transient_context"));
    assert!(!read_tool_content.contains(page_text_marker));
    let restored_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut restored_session = Session::load_from_store("parent", "model", restored_store)?;
    let restored_request = restored_session.build_request(
        temp.path(),
        &MemoryConfig { enabled: false },
        Vec::new(),
        None,
        None,
        None,
    )?;
    assert!(
        restored_request.messages.iter().all(|message| {
            message
                .content
                .as_deref()
                .is_none_or(|content| !content.contains(page_text_marker))
        }),
        "restored parent request must not replay transient child page text"
    );
    Ok(())
}

#[tokio::test]
async fn spawn_agent_records_budget_warning_without_failing_completed_child() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_agent_tokens_per_task = 10;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(UsageProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let call = ToolCall {
        id: "call-expensive".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &call,
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(!result.is_error());
    assert!(result.content.contains("expensive child done"));
    let thread_id =
        chat_agent_thread_id_for_call(&call.id, &sigil_kernel::AgentProfileId::new("explore")?)?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("thread projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(message) if message.contains("agent budget warning after child completion"))
    }));
    Ok(())
}

#[tokio::test]
async fn spawn_agent_enforces_max_fanout() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_spawn_fanout_per_turn = 0;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-fanout".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(result.is_error());
    let model_content: serde_json::Value = serde_json::from_str(&result.to_model_content())?;
    assert!(
        model_content["error"]["details"]
            .get("requires_user_decision")
            .is_none()
    );
    assert_eq!(
        model_content["error"]["details"]["do_not_self_complete_delegated_scope"],
        true
    );
    assert_eq!(
        model_content["error"]["details"]["config_paths"][0],
        "[task].max_spawn_fanout_per_turn"
    );
    assert!(
        result
            .metadata
            .details
            .get("requires_user_decision")
            .is_none()
    );
    let thread_id = chat_agent_thread_id_for_call(
        "call-fanout",
        &sigil_kernel::AgentProfileId::new("explore")?,
    )?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("failed thread projected");
    assert_eq!(thread.status, AgentThreadStatus::Failed);
    assert!(
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("fan-out budget"))
    );
    Ok(())
}

async fn spawned_runtime_session()
-> Result<(AgentToolRuntime, Session, sigil_kernel::AgentThreadId)> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry.clone(),
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let call = ToolCall {
        id: "call-spawn-direct".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };
    let _ = runtime
        .handle_agent_tool_call(
            &mut session,
            &call,
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let thread_id =
        chat_agent_thread_id_for_call(&call.id, &sigil_kernel::AgentProfileId::new("explore")?)?;
    Ok((runtime, session, thread_id))
}

fn supervisor(config: &RootConfig) -> Result<AgentSupervisor> {
    Ok(AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(config)?,
        AgentBudgetPolicy::from_root_config(config),
        provider_capabilities(),
    ))
}

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(4),
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: false },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
                "model": "deepseek-v4-flash",
            }),
        )]),
        mcp_servers: Vec::new(),
    }
}

fn run_options(workspace_root: PathBuf) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: Some(4),
        tool_timeout_secs: 30,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: sigil_kernel::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    }
}

fn provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}
