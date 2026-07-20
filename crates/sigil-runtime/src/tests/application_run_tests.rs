use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    AgentRunOutcome, AgentRunOutput, AgentRunResult, AgentRunTerminalReason, ApprovalHandler,
    AssistantMessageKind, AutoApproveHandler, ControlEntry, DisclosurePresentationError,
    DisclosurePresentationReceipt, EgressDisclosurePresenter, JsonlSessionStore, ModelMessage,
    PreEgressDisclosure, PublicRunEvent, PublicRunEventKind, ReasoningEffort, RootConfig,
    RunCancellationOwner, RunCancellationTerminalOutcome, RunEvent, Session, SessionLogEntry,
    TaskId, TaskStepId, TaskVerificationRerunRequest, Tool, ToolAccess, ToolApproval, ToolCall,
    ToolCategory, ToolContext, ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolSpec, UsageStats,
};

use super::{
    ApplicationRunControl, ApplicationRunEventHandler, ApplicationRunEventSequence,
    ApplicationRunInteraction, ApplicationRunPrepareError, ApplicationRunPrepareErrorClass,
    ApplicationRunRequest, ApplicationRunServices, ApplicationRunTerminalStatus,
    ApplicationSessionLeaseManager, ApplicationTranscriptRole,
    MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES, PublicApplicationEventBridge,
    admit_application_reasoning_effort, admit_application_skill_binding,
    application_run_context_view, application_run_input, application_session_transcript_page,
    application_terminal_projection, application_verification_view,
    attach_application_request_context, bind_application_session,
    bind_application_session_with_model, bind_existing_application_session,
    constrain_application_tool_registry, default_application_session_path,
    optional_eager_mcp_warning, record_application_preparation_cancellation,
    rerun_application_verification, validate_execution_contract,
};

struct RejectingDisclosurePresenter;

#[async_trait]
impl EgressDisclosurePresenter for RejectingDisclosurePresenter {
    async fn present(
        &self,
        _disclosure: PreEgressDisclosure,
    ) -> std::result::Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        Err(DisclosurePresentationError::SinkClosed)
    }
}

struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.to_owned(),
            description: "application scope test tool".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.0,
            "ok",
            ToolResultMeta::default(),
        ))
    }
}

#[test]
fn application_tool_scope_is_exact_and_rejects_unknown_names() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedTool("read_file")));
    registry.register(Arc::new(NamedTool("bash")));
    let scope =
        ToolRegistryScope::from_names_and_prefixes(["read_file"], std::iter::empty::<&str>());
    let scoped = constrain_application_tool_registry(registry.clone(), &scope)
        .expect("known exact scope should apply");
    assert!(scoped.spec_for("read_file").is_some());
    assert!(scoped.spec_for("bash").is_none());

    let unknown =
        ToolRegistryScope::from_names_and_prefixes(["missing_tool"], std::iter::empty::<&str>());
    let error = match constrain_application_tool_registry(registry, &unknown) {
        Ok(_) => panic!("unknown tool scope must fail before dispatch"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unknown tool"));
}

#[test]
fn session_lease_rejects_overlapping_foreground_runs_and_releases_on_drop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("sessions/session.jsonl");
    let manager = ApplicationSessionLeaseManager::new();

    let first = manager.acquire(&path)?;
    let error = manager
        .acquire(&path)
        .expect_err("same durable session must have one foreground run");
    assert!(error.to_string().contains("active foreground run"));

    drop(first);
    let reacquired = manager.acquire(&path)?;
    drop(reacquired);
    Ok(())
}

#[tokio::test]
async fn verification_view_uses_durable_truth_and_rerun_shares_the_foreground_lease() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let session_path = temp.path().join("state/sessions/verification.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    assert!(application_verification_view(&binding.session_log_path)?.is_none());

    let lease_manager = Arc::new(ApplicationSessionLeaseManager::new());
    let foreground = lease_manager.acquire(&binding.session_log_path)?;
    let services = ApplicationRunServices::with_session_leases(
        Arc::new(RejectingDisclosurePresenter),
        Arc::clone(&lease_manager),
    );
    let request = TaskVerificationRerunRequest {
        task_id: TaskId::new("task_1")?,
        step_id: TaskStepId::new("verify_1")?,
        check_spec_id: "cargo-test".to_owned(),
        check_spec_hash: "check-hash".to_owned(),
        policy_hash: "policy-hash".to_owned(),
        workspace_snapshot_id: "snapshot-1".to_owned(),
    };

    let error = rerun_application_verification(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
        &services,
        &request,
    )
    .await
    .expect_err("verification must not overlap another foreground operation");

    assert!(error.to_string().contains("active foreground run"));
    drop(foreground);
    Ok(())
}

#[cfg(unix)]
#[test]
fn session_lease_collapses_symlink_aliases_to_one_canonical_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let real = temp.path().join("real-session.jsonl");
    let alias = temp.path().join("alias-session.jsonl");
    std::fs::File::create(&real)?;
    std::os::unix::fs::symlink(&real, &alias)?;
    let manager = ApplicationSessionLeaseManager::new();

    let first = manager.acquire(&real)?;
    let error = manager
        .acquire(&alias)
        .expect_err("symlink alias must resolve to the active durable session");
    assert!(error.to_string().contains("active foreground run"));
    drop(first);
    Ok(())
}

#[test]
fn default_session_path_and_repo_context_are_application_owned() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("README.md"), "Sigil application service")?;

    let path = default_application_session_path(&temp.path().join("sessions"));
    let input = application_run_input(temp.path(), "summarize README.md".to_owned());

    assert!(path.starts_with(temp.path().join("sessions")));
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("jsonl")
    );
    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "repo-file:README.md")
    );
    Ok(())
}

#[tokio::test]
async fn application_request_context_uses_runtime_resolver() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("README.md"), "Sigil application resolver")?;
    let resolver = crate::RequestContextResolver::request_local(temp.path().to_path_buf());

    let input = attach_application_request_context(
        sigil_kernel::AgentRunInput::user("summarize README.md"),
        &resolver,
        "summarize README.md",
    )
    .await;

    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "repo-file:README.md")
    );
    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "lsp-context:unavailable")
    );
    Ok(())
}

#[test]
fn adapter_session_binding_creates_and_reopens_the_same_durable_v2_scope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;
    let requested_path = temp.path().join("state/sessions/http.jsonl");

    let first = bind_application_session(&config_path, temp.path(), Some(&requested_path))?;
    let second = bind_application_session(&config_path, temp.path(), Some(&requested_path))?;

    assert_eq!(first, second);
    assert!(first.session_log_path.is_absolute());
    assert!(first.session_log_path.exists());
    assert!(!first.session_scope_id.is_empty());
    Ok(())
}

#[test]
fn adapter_session_binding_accepts_only_offered_models_for_new_durable_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let selected_path = temp.path().join("state/sessions/pro.jsonl");

    let binding = bind_application_session_with_model(
        &config_path,
        temp.path(),
        Some(&selected_path),
        Some("deepseek-v4-pro"),
    )?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(context.model_name, "deepseek-v4-pro");
    assert!(
        context
            .available_models
            .contains(&"deepseek-v4-flash".to_owned())
    );
    assert!(
        context
            .available_models
            .contains(&"deepseek-v4-pro".to_owned())
    );

    let rejected = bind_application_session_with_model(
        &config_path,
        temp.path(),
        Some(&temp.path().join("state/sessions/unknown.jsonl")),
        Some("unknown-model"),
    );
    assert!(matches!(
        rejected,
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn session_reopen_binding_requires_an_existing_durable_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let session_path = temp.path().join("state/sessions/existing.jsonl");
    let created = bind_application_session(&config_path, temp.path(), Some(&session_path))?;

    let reopened = bind_existing_application_session(&config_path, &created.session_log_path)?;

    assert_eq!(reopened, created);
    let missing = temp.path().join("state/sessions/missing.jsonl");
    assert!(bind_existing_application_session(&config_path, &missing).is_err());
    assert!(!missing.exists());
    Ok(())
}

#[test]
fn run_context_uses_durable_identity_and_only_proven_usage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let session_path = temp.path().join("state/sessions/context.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;

    let empty = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(empty.provider_name, "deepseek");
    assert_eq!(empty.model_name, "deepseek-v4-flash");
    assert_eq!(
        empty.default_permission_mode,
        sigil_kernel::PermissionMode::Manual
    );
    assert_eq!(empty.available_models.len(), 2);
    assert_eq!(
        empty.available_reasoning_efforts,
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );
    assert_eq!(empty.default_reasoning_effort, Some(ReasoningEffort::Max));
    assert!(empty.reasoning_effort_binding.is_some());
    assert_eq!(empty.context_window_tokens, Some(1_000_000));
    assert_eq!(
        empty.context_window_source,
        crate::ContextWindowSource::Provider
    );
    assert_eq!(empty.last_prompt_tokens, None);
    assert_eq!(
        empty.extension_catalog.commands.len(),
        crate::APPLICATION_COMMANDS.len()
    );
    assert!(
        empty
            .extension_catalog
            .commands
            .iter()
            .any(|command| command.canonical == "/new" && command.available)
    );
    assert!(
        empty
            .extension_catalog
            .agents
            .iter()
            .all(|agent| !agent.available && agent.unavailable_reason.is_some())
    );

    JsonlSessionStore::new(&binding.session_log_path)?.append(&SessionLogEntry::Control(
        ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 42_000,
            completion_tokens: 800,
            cache_hit_tokens: 30_000,
            cache_miss_tokens: 12_000,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }),
    ))?;
    let used = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(used.last_prompt_tokens, Some(42_000));
    assert!(
        application_run_context_view(
            &config_path,
            temp.path(),
            &binding.session_log_path,
            "wrong-scope",
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn exact_inline_skill_binding_loads_transient_context_and_audit_entry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let skill_path = temp.path().join(".sigil/skills/review/SKILL.md");
    std::fs::create_dir_all(skill_path.parent().expect("skill parent"))?;
    std::fs::write(
        &skill_path,
        r#"---
id: review
name: Review
description: Review the selected code.
trust: trusted
run-as: inline
user-invocable: true
---

# Review instructions
Inspect the requested code and report concrete findings.
"#,
    )?;

    let root_config = RootConfig::load(&config_path)?;
    let report = crate::discover_skill_index(temp.path(), &root_config.skills)?;
    let descriptor = report
        .snapshot
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id == "review")
        .expect("review skill should be discovered");
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "review src/lib.rs",
        "run-skill",
    );
    request.skill_binding = Some(crate::ApplicationSkillBinding {
        skill_id: descriptor.id.clone(),
        skill_sha256: descriptor.sha256.clone(),
        index_fingerprint: report.snapshot.fingerprint.clone(),
    });
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

    let loaded =
        admit_application_skill_binding(&request, &root_config, temp.path(), &mut session)?
            .expect("exact binding should load");

    assert_eq!(loaded.descriptor.id, "review");
    assert!(
        loaded
            .transient_context
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Review instructions"))
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SkillLoaded(loaded))
            if loaded.skill_id == "review" && loaded.run_id.as_deref() == Some("run-skill")
    )));

    request
        .skill_binding
        .as_mut()
        .expect("binding should exist")
        .index_fingerprint = "stale".to_owned();
    assert!(matches!(
        admit_application_skill_binding(&request, &root_config, temp.path(), &mut session,),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn explicit_reasoning_effort_requires_exact_current_binding() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let config = RootConfig::load(&config_path)?;
    let supported = crate::reasoning_effort::supported_reasoning_efforts(
        &config.agent.provider,
        &config.agent.model,
    );
    let binding = crate::reasoning_effort::reasoning_effort_binding(
        &config.agent.provider,
        &config.agent.model,
        &supported,
    )
    .expect("default model supports reasoning effort");
    let mut request =
        ApplicationRunRequest::non_interactive("sigil.toml", ".", "hello", "run-effort");
    request.reasoning_effort = Some(ReasoningEffort::High);
    request.reasoning_effort_binding = Some(binding);
    assert!(admit_application_reasoning_effort(&request, &config).is_ok());

    request.reasoning_effort_binding = Some("stale".to_owned());
    assert!(matches!(
        admit_application_reasoning_effort(&request, &config),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));

    request.reasoning_effort = None;
    assert!(matches!(
        admit_application_reasoning_effort(&request, &config),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn transcript_page_is_scope_checked_chronological_bounded_and_argument_free() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let session_path = temp.path().join("state/sessions/transcript.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("first")))?;
    store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            Some("checking".to_owned()),
            vec![ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{\"token\":\"must-not-project\"}".to_owned(),
            }],
            AssistantMessageKind::ToolPreamble,
        ),
    ))?;
    store.append(&SessionLogEntry::ToolResult(ModelMessage::tool(
        "call-1",
        "tool output",
    )))?;
    store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            Some("final".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let latest = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        2,
    )?;
    assert_eq!(latest.total_messages, 4);
    assert_eq!(latest.messages.len(), 2);
    assert_eq!(latest.messages[0].ordinal, 3);
    assert_eq!(latest.messages[0].role, ApplicationTranscriptRole::Tool);
    assert_eq!(latest.messages[0].tool_name.as_deref(), Some("read_file"));
    assert_eq!(latest.messages[1].content.as_deref(), Some("final"));
    assert_eq!(latest.next_before, Some(3));
    assert!(!format!("{latest:?}").contains("must-not-project"));

    let older = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        latest.next_before,
        2,
    )?;
    assert_eq!(
        older
            .messages
            .iter()
            .map(|message| message.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(older.next_before, None);
    assert!(
        application_session_transcript_page(&binding.session_log_path, "wrong-scope", None, 10)
            .is_err()
    );
    Ok(())
}

#[test]
fn transcript_page_truncates_utf8_content_without_breaking_character_boundaries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let session_path = temp.path().join("state/sessions/large-transcript.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let original = "界".repeat(MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES);
    store.append(&SessionLogEntry::User(ModelMessage::user(&original)))?;

    let page = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        1,
    )?;
    let message = &page.messages[0];
    let content = message.content.as_deref().expect("text remains available");
    assert!(message.truncated);
    assert_eq!(message.original_content_bytes, original.len() as u64);
    assert!(content.len() <= MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES);
    assert!(content.is_char_boundary(content.len()));
    Ok(())
}

#[test]
fn preparation_cancellation_is_durable_idempotent_and_secret_safe() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )?;
    let session_path = temp.path().join("state/sessions/http.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;

    let first = record_application_preparation_cancellation(
        &config_path,
        &binding.session_log_path,
        "run-1",
        "stop token=super-secret",
    )?;
    let second = record_application_preparation_cancellation(
        &config_path,
        &binding.session_log_path,
        "run-1",
        "stop token=super-secret",
    )?;

    assert_eq!(first, binding);
    assert_eq!(second, binding);
    let durable = std::fs::read_to_string(&binding.session_log_path)?;
    assert_eq!(durable.matches("cancel-preparation-run-1").count(), 2);
    assert!(durable.contains("\"outcome\":\"cancelled\""));
    assert!(durable.contains("token=[redacted]"));
    assert!(!durable.contains("super-secret"));
    Ok(())
}

#[test]
fn interaction_contract_distinguishes_noninteractive_and_external_surfaces() {
    assert_eq!(
        ApplicationRunInteraction::NonInteractive.kernel_mode(),
        sigil_kernel::InteractionMode::Headless
    );
    assert_eq!(
        ApplicationRunInteraction::AdapterManaged.kernel_mode(),
        sigil_kernel::InteractionMode::Interactive
    );
    assert_eq!(
        ApplicationRunInteraction::ExternallyInteractive.kernel_mode(),
        sigil_kernel::InteractionMode::Interactive
    );
}

#[test]
fn prepare_error_class_is_typed_and_public_message_does_not_expose_source() {
    let error = ApplicationRunPrepareError::configuration(anyhow::anyhow!(
        "secret provider value must remain in the source chain"
    ));

    assert_eq!(
        error.class(),
        ApplicationRunPrepareErrorClass::Configuration
    );
    assert_eq!(error.to_string(), "application configuration is invalid");
    assert!(!error.to_string().contains("secret provider value"));
}

#[test]
fn optional_eager_mcp_warning_redacts_known_and_structural_secret_carriers() {
    let redactor = sigil_kernel::SecretRedactor::from_values(["known-secret-value"]);
    let error =
        anyhow::anyhow!("Authorization: Bearer known-secret-value; api_key=another-secret-value");

    let warning = optional_eager_mcp_warning(&redactor, "optional-server", &error);

    assert!(warning.contains("optional eager MCP server optional-server failed"));
    assert!(!warning.contains("known-secret-value"));
    assert!(!warning.contains("another-secret-value"));
    assert!(warning.contains("[redacted]"));
}

struct ExplicitApprovalHandler;

impl ApprovalHandler for ExplicitApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Approve)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

#[test]
fn externally_interactive_runs_reject_automated_approval_handlers() {
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::AdapterManaged,
            &AutoApproveHandler,
            true,
        )
        .is_ok()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::AdapterManaged,
            &AutoApproveHandler,
            false,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &AutoApproveHandler,
            true,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &ExplicitApprovalHandler,
            false,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &ExplicitApprovalHandler,
            true,
        )
        .is_ok()
    );
}

#[test]
fn public_event_bridge_sequences_lifecycle_and_kernel_events() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let mut recorder = Recorder::default();
    let events = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder);
    bridge.emit(PublicRunEventKind::RunStarted {
        prompt: "hello".to_owned(),
    })?;
    sigil_kernel::EventHandler::handle(&mut bridge, RunEvent::TextDelta("hi".to_owned()))?;
    bridge.emit(PublicRunEventKind::RunFinished {
        final_text: "hi".to_owned(),
    })?;
    drop(bridge);

    assert_eq!(
        recorder
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(matches!(
        recorder.0[0].event,
        PublicRunEventKind::RunStarted { .. }
    ));
    assert!(matches!(
        recorder.0[1].event,
        PublicRunEventKind::TextDelta { .. }
    ));
    assert!(matches!(
        recorder.0[2].event,
        PublicRunEventKind::RunFinished { .. }
    ));
    Ok(())
}

#[test]
fn public_event_sequence_seals_after_root_terminal() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let sequence = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut recorder = Recorder::default();
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    )?;
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunFailed {
            error: "interrupted".to_owned(),
        },
    )?;
    assert!(
        sequence
            .emit(
                &mut recorder,
                PublicRunEventKind::TextDelta {
                    text: "late".to_owned(),
                },
            )
            .is_err()
    );
    assert_eq!(recorder.0.len(), 2);
    Ok(())
}

#[test]
fn failed_terminal_delivery_does_not_seal_the_public_event_sequence() -> Result<()> {
    struct FailFirstTerminal {
        failed: bool,
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for FailFirstTerminal {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            if !self.failed && matches!(event.event, PublicRunEventKind::RunFailed { .. }) {
                self.failed = true;
                anyhow::bail!("durable publication failed");
            }
            self.events.push(event);
            Ok(())
        }
    }

    let sequence = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut handler = FailFirstTerminal {
        failed: false,
        events: Vec::new(),
    };
    assert!(
        sequence
            .emit(
                &mut handler,
                PublicRunEventKind::RunFailed {
                    error: "first terminal".to_owned(),
                },
            )
            .is_err()
    );
    sequence.emit(
        &mut handler,
        PublicRunEventKind::RunFailed {
            error: "retry terminal".to_owned(),
        },
    )?;

    assert_eq!(handler.events.len(), 1);
    assert_eq!(handler.events[0].sequence, 1);
    Ok(())
}

#[test]
fn non_final_kernel_terminals_do_not_project_as_run_finished() {
    for (terminal_reason, expected_status) in [
        (
            AgentRunTerminalReason::MaxTurns,
            ApplicationRunTerminalStatus::Interrupted,
        ),
        (
            AgentRunTerminalReason::DelegationUnsatisfied,
            ApplicationRunTerminalStatus::Blocked,
        ),
    ] {
        let output = AgentRunOutput {
            result: AgentRunResult {
                final_text: String::new(),
                tool_calls: 0,
                final_message_id: None,
            },
            outcome: AgentRunOutcome {
                terminal_reason,
                ..AgentRunOutcome::default()
            },
        };
        let (status, event) = application_terminal_projection(&output);

        assert_eq!(status, expected_status);
        assert!(matches!(event, PublicRunEventKind::RunFailed { .. }));
    }
}

#[tokio::test]
async fn cancellation_control_persists_request_then_terminal_after_quiescence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(
            ApplicationSessionLeaseManager::new().acquire(&temp.path().join("session.jsonl"))?,
        ),
    };
    let unblocked = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&unblocked);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let ticket = control.request_cancellation("test cancel", None, move || {
        signal.store(true, Ordering::SeqCst);
    })?;
    assert!(unblocked.load(Ordering::SeqCst));
    assert!(control.handle().is_cancel_requested());
    drop(root_task_guard);

    let outcome = control
        .finalize_cancellation(ticket, true, &mut events)
        .await?;
    assert_eq!(outcome, RunCancellationTerminalOutcome::Cancelled);
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunCancelled)
    ));
    Ok(())
}

#[tokio::test]
async fn cancellation_without_execution_join_persists_interrupted_and_failed_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = control.request_cancellation(
        "test interrupted terminal",
        Some(std::time::Duration::from_millis(10)),
        || {},
    )?;
    drop(root_task_guard);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let outcome = control
        .finalize_cancellation(ticket, false, &mut events)
        .await?;

    assert_eq!(outcome, RunCancellationTerminalOutcome::Interrupted);
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    let durable = std::fs::read_to_string(store_path)?;
    assert!(durable.contains("\"outcome\":\"interrupted\""));
    Ok(())
}

#[tokio::test]
async fn cancellation_audit_failure_still_unblocks_and_requires_failed_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    temp.close()?;
    let unblocked = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&unblocked);

    let error = control
        .request_cancellation("test audit failure", None, move || {
            signal.store(true, Ordering::SeqCst);
        })
        .expect_err("removed session parent must reject the durable append");
    assert!(unblocked.load(Ordering::SeqCst));
    assert!(control.handle().is_cancel_requested());
    let ticket = error
        .into_ticket()
        .expect("activated cancellation must return a cleanup ticket");
    drop(root_task_guard);

    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();
    assert!(
        control
            .finalize_cancellation(ticket, true, &mut events)
            .await
            .is_err()
    );
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    Ok(())
}
