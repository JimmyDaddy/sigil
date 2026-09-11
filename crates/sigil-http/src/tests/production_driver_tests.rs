use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sigil_application::{
    ApplicationCommand, ApplicationCommandReceipt, ApplicationPermissionMode,
    ApplicationQueueAction, ApplicationQueueItemKind, ApplicationRecoveryAction,
    ConversationCommand, McpCommand, RunStartOptions, SafeText,
};
use sigil_kernel::{
    AgentRole, ApprovalMode, AssistantMessageKind, CandidateCheck, CheckCommand,
    CheckDiscoverySource, CheckPromotion, CheckSpecRecordedEntry, CompletionCriteria, ControlEntry,
    EvidenceScope, JsonlSessionStore, NetworkEffect, PermissionConfirmation, PermissionDecision,
    PlanDraftCreatedEntry, PublicRunEvent, PublicRunEventKind, ReadinessEvaluatedEntry,
    ReadinessEvaluation, RequiredAction, RunStatus, SessionLogEntry, SessionRef, TaskId,
    TaskPauseRequest, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus, ToolAccess, ToolApproval,
    ToolApprovalSessionGrantUnavailableReason, ToolApprovalSessionGrantUnavailableReasonCode,
    ToolArtifactDescriptorV1, ToolArtifactEncoding, ToolArtifactSensitivity, ToolArtifactStore,
    ToolCall, ToolCategory, ToolEffect, ToolPreviewCapability, ToolResult, ToolResultMeta,
    ToolResultRecordedV3, ToolSpec, ToolSubject, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationProductAction, VerificationVerdict,
    VisibleCompletionState, build_workspace_snapshot, stable_workspace_id,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use crate::{
    HttpCommandEnvelope, HttpConversationQueueBlockedReason, HttpConversationQueueCommandAction,
    HttpConversationQueueCommandRequest, HttpConversationQueueDriverCommand,
    HttpConversationQueueDriverError, HttpConversationQueueItemKind,
    HttpConversationQueuePromptMaterial, HttpDurableEgressDisclosureJournal,
    HttpDurableProtocolJournal, HttpForegroundRunOwner, HttpPermissionMode, HttpPlanDecisionAction,
    HttpPlanDecisionRequest, HttpRunStartRequest, HttpRunStatus, HttpSessionCreateRequest,
    HttpSessionOpenRequest, HttpSessionSnapshot, HttpTaskContinuationRequest,
    HttpUserInputDecisionRequest,
};

#[path = "production_projection_owner_tests.rs"]
mod projection_owner;

#[path = "production_artifact_access_tests.rs"]
mod artifact_access;

#[path = "production_direct_delivery_tests.rs"]
mod direct_delivery;

#[path = "production_plan_recovery_tests.rs"]
mod plan_recovery;

#[test]
fn preparation_failure_projects_typed_route_recovery_without_string_parsing() {
    let error = anyhow::Error::new(
        sigil_runtime::application_run::ApplicationRunPrepareError::SessionRouteConfirmationRequired {
            recovery_binding: "route-binding-1".to_owned(),
        },
    );
    assert!(matches!(
        public_preparation_failure_event(&error),
        PublicRunEventKind::RouteRecoveryRequired {
            code: PublicRouteRecoveryCode::SessionRouteConfirmationRequired,
            recovery_binding,
            retryable: true,
            ..
        } if recovery_binding == "route-binding-1"
    ));
}

#[test]
fn preparation_authority_failure_projects_new_session_without_provider_fallback() {
    let error = anyhow::Error::new(
        sigil_runtime::application_run::ApplicationRunPrepareError::AuthorityUnavailable {
            source: anyhow::anyhow!("durable authority failed"),
        },
    );
    assert!(matches!(
        public_preparation_failure_event(&error),
        PublicRunEventKind::RouteRecoveryRequired {
            code: PublicRouteRecoveryCode::AuthorityUnavailable,
            actions,
            retryable: true,
            ..
        } if actions == vec![
            PublicRouteRecoveryAction::StartNewSession,
            PublicRouteRecoveryAction::BackToSessionLibrary,
        ]
    ));
}

fn call() -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"README.md"}"#.to_owned(),
    }
}

fn unavailable_session_grant_reason() -> Option<ToolApprovalSessionGrantUnavailableReason> {
    Some(ToolApprovalSessionGrantUnavailableReason {
        code: ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
    })
}

fn spec(access: ToolAccess, network_effect: Option<NetworkEffect>) -> ToolSpec {
    ToolSpec {
        name: "read_file".to_owned(),
        description: "read a file".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access,
        network_effect,
        preview: ToolPreviewCapability::None,
    }
}

fn approval_identity() -> ApprovalRequestIdentityV2 {
    ApprovalRequestIdentityV2 {
        session_id: "durable-session-1".to_owned(),
        run_id: "run-1".to_owned(),
        call_id: "call-1".to_owned(),
        approval_request_id: "kernel-approval-v2:request-1".to_owned(),
        plan_hash: "a".repeat(64),
        policy_version: "permission-policy-v2".to_owned(),
        execution_binding_hash: "b".repeat(64),
        expires_at_ms: current_unix_time_ms().saturating_add(60_000),
    }
}

fn approval_context() -> ToolApprovalContext {
    let identity = approval_identity();
    ToolApprovalContext {
        requested_at_ms: current_unix_time_ms(),
        expires_at_ms: identity.expires_at_ms,
        identity,
        permission_signature: "permission-signature".to_owned(),
        policy_fingerprint: "policy-fingerprint".to_owned(),
    }
}

#[test]
fn task_guidance_queue_kind_stays_private_because_continuation_is_a_typed_run_command() {
    assert_eq!(
        kernel_queue_kind_to_http(sigil_kernel::ConversationInputKind::TaskGuidance),
        HttpConversationQueueItemKind::Unknown
    );
}

struct ControlledPreparation {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
}

#[derive(Default)]
struct FailingQueuedPreparation {
    queued_calls: AtomicUsize,
}

#[derive(Default)]
struct FailingTaskPreparation {
    requests: Mutex<Vec<ApplicationTaskContinuationRequest>>,
}

fn write_production_test_config(path: &std::path::Path, workspace_root: &str) {
    let storage = isolated_storage_toml(path);
    let config = format!(
        r#"config_version = 2

[workspace]
root = "{workspace_root}"

{storage}

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = {{ source = "none" }}
"#
    );
    std::fs::write(path, config).expect("production test config should write");
}

#[tokio::test]
async fn conversation_display_keeps_task_selection_out_of_auxiliary_history_reads() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let driver = production_queue_driver(&fixture, "display-task-selection");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let page = registry.conversation_display_page(&session.id, None, 20)?;
    let config_path = fixture.path().join("sigil.toml");
    let base = std::fs::read_to_string(&config_path)?;
    std::fs::write(fixture.path().join("visible.txt"), "snapshot content")?;
    for composition in [
        "\n[composition]\nprofile = 'core'\n[task]\nenabled = 'deferred invalid value'\n",
        "\n[composition]\nprofile = 'core'\nenhancements = ['task_orchestration']\n[task]\nenabled = false\n",
        "\n[composition]\nprofile = 'standard'\n[task]\nenabled = false\n",
        "\n[composition]\nprofile = 'core'\nenhancements = ['task_orchestration']\n[task]\nenabled = true\n",
    ] {
        std::fs::write(&config_path, format!("{base}{composition}"))?;
        assert_eq!(
            registry.conversation_display_page(&session.id, None, 20)?,
            page,
            "historical display must not load Task configuration or scan the workspace"
        );
    }
    Ok(())
}

fn write_production_preparation_failure_config(path: &std::path::Path, workspace_root: &str) {
    write_production_test_config(path, workspace_root);
    let mut config =
        std::fs::read_to_string(path).expect("production test config should be readable");
    // Root-config parsing and read-only route projection deliberately accept this. Provider
    // construction is the first owner that resolves request timeouts, so this drives the real
    // pre-Started preparation branch without a mock preparer or network dependency.
    config.push_str(
        r#"

[model_request]
request_timeout_secs = 0
stream_idle_timeout_secs = 1
"#,
    );
    std::fs::write(path, config).expect("preparation failure config should write");
}

fn write_production_bounded_network_failure_config(path: &std::path::Path, workspace_root: &str) {
    write_production_test_config(path, workspace_root);
    let mut config =
        std::fs::read_to_string(path).expect("production test config should be readable");
    // Keep the real unreachable endpoint, but bound both the transport and its durable recovery
    // policy. The resulting terminal is a paused recovery exhaustion, not a fabricated failure.
    config.push_str(
        r#"

[model_request]
request_timeout_secs = 1
stream_idle_timeout_secs = 1
stream_total_timeout_secs = 1

[recovery.provider]
max_transport_retries = 0
max_partial_output_retries = 0
initial_delay_ms = 0
max_delay_ms = 0
jitter_ratio = 0.0
max_cumulative_delay_ms = 0
"#,
    );
    std::fs::write(path, config).expect("bounded network failure config should write");
}

fn write_reasoning_test_config(path: &std::path::Path) {
    let storage = isolated_storage_toml(path);
    let config = format!(
        r#"config_version = 2

[workspace]
root = "."

{storage}

[agent]
connection = "deepseek-test"
model = "deepseek-v4-flash"

[connections.deepseek-test]
label = "DeepSeek test"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#
    );
    std::fs::write(path, config).expect("reasoning test config should write");
}

fn toml_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn isolated_storage_toml(config_path: &std::path::Path) -> String {
    let root = config_path
        .parent()
        .expect("production test config should have a parent");
    // TOML basic strings treat Windows backslashes as escape introducers (for example, `\\U`).
    // Forward slashes are accepted by Windows path APIs and keep the generated fixture valid on
    // every host platform.
    format!(
        "[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"",
        toml_path(&root.join("state")),
        toml_path(&root.join("cache"))
    )
}

fn production_test_model_route(
    temp: &tempfile::TempDir,
) -> (String, sigil_kernel::ResolvedModelRoute) {
    let config = sigil_kernel::RootConfig::load(&temp.path().join("sigil.toml"))
        .expect("config should load");
    sigil_runtime::provider_connections::resolve_default_model_route(&config)
        .expect("V2 model route should resolve")
}

fn production_queue_driver(
    temp: &tempfile::TempDir,
    journal_suffix: &str,
) -> Arc<HttpProductionRunDriver> {
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(
            temp.path().join(format!("protocol-{journal_suffix}.json")),
            16,
        )
        .expect("queue protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path()
                .join(format!("disclosures-{journal_suffix}.json")),
            16,
        )
        .expect("queue disclosure journal should initialize"),
    );
    Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production queue driver should initialize"),
    )
}

fn production_queue_session(temp: &tempfile::TempDir) -> HttpSessionSnapshot {
    production_queue_session_named(temp, "queue-session")
}

#[tokio::test]
async fn production_driver_attaches_the_shared_application_task_executor() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "task-executor");

    assert!(driver.services.task_executor_attached());
}

#[tokio::test]
async fn production_http_application_client_uses_runtime_projection_page_and_reservation() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "application-port");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-application.json"), 16)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let queue_generation = registry
        .conversation_queue(&session.id)
        .expect("queue should be readable")
        .generation
        .0;
    let client = registry
        .application_client(&session.id, "http-application-test")
        .expect("application client should bind");
    let (projection, page, receipt, start_receipt, queue_receipt, recovery_receipt) =
        tokio::task::spawn_blocking(move || {
            let projection = client
                .refresh()
                .expect("application projection should refresh");
            let page = client
                .page(None, 1)
                .expect("application page should use the same frontier");
            let receipt = client
                .execute(
                    "application-unsupported-command",
                    ApplicationCommand::Mcp(McpCommand::Refresh {
                        binding: "test-server".to_owned(),
                    }),
                )
                .expect("unsupported command should receive a typed rejection");
            let start_receipt = client
                .execute(
                    "application-start-test",
                    ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                        prompt: Some(
                            SafeText::new("start through application port").expect("prompt"),
                        ),
                        options: Some(Box::new(RunStartOptions {
                            permission_mode: ApplicationPermissionMode::Manual,
                            model: None,
                            route_recovery_binding: None,
                            reasoning_effort: None,
                            reasoning_effort_binding: None,
                            skill: None,
                            agent: None,
                            task_continuation: None,
                        })),
                    }),
                )
                .expect("run start should receive a durable uncertain receipt");
            let queue_receipt = client
                .execute(
                    "application-queue-test",
                    ApplicationCommand::Conversation(ConversationCommand::Queue {
                        expected_generation: SafeText::new(queue_generation).expect("generation"),
                        action: ApplicationQueueAction::Enqueue {
                            target: sigil_application::ApplicationQueueTarget::MainThread,
                            prompt: SafeText::new("queue through application port")
                                .expect("queue prompt"),
                            kind: ApplicationQueueItemKind::Chat,
                            reasoning_effort: None,
                        },
                    }),
                )
                .expect("queue mutation should receive a durable uncertain receipt");
            let recovery_receipt = client
                .execute(
                    "application-recovery-test",
                    ApplicationCommand::Conversation(ConversationCommand::Recovery {
                        action: ApplicationRecoveryAction::PrepareCompaction {
                            preview_id: SafeText::new("preview").expect("preview"),
                        },
                    }),
                )
                .expect("stale recovery binding should receive a typed rejection");
            (
                projection,
                page,
                receipt,
                start_receipt,
                queue_receipt,
                recovery_receipt,
            )
        })
        .await
        .expect("blocking application client task should complete");

    assert_eq!(
        projection.scope.session.as_ref().map(ToString::to_string),
        Some(session.durable_session_scope_id)
    );
    assert_eq!(page.scope, projection.scope);
    assert!(matches!(receipt, ApplicationCommandReceipt::Rejected(_)));
    let ApplicationCommandReceipt::Uncertain(start_receipt) = start_receipt else {
        panic!("run start should be represented as an uncertain application receipt");
    };
    assert_eq!(
        start_receipt.recovery.key.command_id.as_str(),
        "application-start-test"
    );
    assert_eq!(
        start_receipt.recovery.phase,
        sigil_application::CommandLifecyclePhase::EffectStarted
    );
    let ApplicationCommandReceipt::Uncertain(queue_receipt) = queue_receipt else {
        panic!("queue mutation should be represented as an uncertain application receipt");
    };
    assert_eq!(
        queue_receipt.recovery.key.command_id.as_str(),
        "application-queue-test"
    );
    assert_eq!(
        queue_receipt.recovery.phase,
        sigil_application::CommandLifecyclePhase::EffectStarted
    );
    let ApplicationCommandReceipt::Rejected(recovery_rejection) = recovery_receipt else {
        panic!("invalid recovery binding should be represented as a typed rejection");
    };
    assert!(!recovery_rejection.reason.contains("preview boundary"));
}

#[tokio::test]
async fn production_run_admission_reports_external_attachment_before_allocating_a_run() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "attachment-admission");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-attachment.json"), 16)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind without taking write ownership");
    driver
        .session_attachments
        .lock()
        .expect("attachment state should not be poisoned")
        .clear();
    let _external_owner =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session.session_log_path,
        )
        .expect("external controller should own the session attachment");

    let error = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "must not allocate a run".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect_err("external attachment must reject before run allocation");

    let crate::HttpRegistryError::SessionRunRecoveryRequired { recovery } = error else {
        panic!("expected typed session recovery conflict");
    };
    assert_eq!(
        recovery.code,
        crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive
    );
    assert!(recovery.retryable);
    assert!(!recovery.recovery_binding.is_empty());
    assert_eq!(
        registry
            .get_session(&session.id)
            .expect("session should remain registered")
            .run_ids,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn production_session_switch_releases_the_previous_idle_attachment() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "attachment-switch");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-switch.json"), 16)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let first = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("first session should bind");
    let second = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("second session should bind and become the active controller target");

    let first_owner =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &first.session_log_path,
        )
        .expect("switching away must release the previous idle session attachment");
    let second_busy =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &second.session_log_path,
        )
        .expect_err("the current controller target must stay attached");
    assert_eq!(
        second_busy.code(),
        sigil_runtime::interactive_session_attachment::SESSION_ATTACHMENT_BUSY_CODE
    );
    drop(first_owner);
}

#[tokio::test]
async fn production_catalog_mutation_gate_rejects_an_external_attachment_owner() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "catalog-mutation-attachment");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-catalog-mutation.json"), 16)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    driver
        .session_attachments
        .lock()
        .expect("attachment state should not be poisoned")
        .clear();
    let before = std::fs::read(&session.session_log_path).expect("session bytes should read");
    let _external_owner =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session.session_log_path,
        )
        .expect("external controller should attach");

    let error = driver
        .acquire_durable_session_mutation_attachment(
            &session.durable_session_scope_id,
            std::path::Path::new(&session.session_log_path),
        )
        .expect_err("catalog mutation must reject the external owner");
    assert!(matches!(
        error,
        HttpRunAdmissionError::SessionAlreadyActive { recovery_binding }
            if !recovery_binding.is_empty()
    ));
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("session bytes should remain readable"),
        before
    );
}

#[tokio::test]
async fn production_selection_recovery_requires_exact_route_and_catalog_bindings() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "selection-admission");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-selection.json"), 16)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("original route should bind");
    let replacement_config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&replacement_config_path);
    std::fs::write(
        &replacement_config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

{storage}

[agent]
connection = "replacement"
model = "gpt-replacement"

[connections.replacement]
label = "Replacement"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:2"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("replacement config should write");
    let context = driver
        .run_context_view(&session)
        .expect("replacement run context should project");
    assert!(matches!(
        context
            .route_recovery
            .as_ref()
            .map(|recovery| recovery.code),
        Some(crate::HttpSessionRouteRecoveryCode::SessionRouteSelectionRequired)
    ));
    let replacement = context
        .model_options
        .iter()
        .find(|option| option.availability != "configured_unavailable")
        .expect("replacement option should be selectable")
        .model_ref
        .clone();
    let route_recovery_binding = context
        .route_recovery
        .as_ref()
        .expect("selection recovery should remain projected")
        .recovery_binding
        .clone();

    let missing_route_error = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "must not allocate without the exact route binding".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: Some(replacement.clone()),
                model_selection_binding: Some(context.model_selection_binding.clone()),
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect_err("missing route binding must reject synchronously");
    assert!(matches!(
        missing_route_error,
        crate::HttpRegistryError::SessionRunRecoveryRequired { recovery }
            if recovery.code
                == crate::HttpSessionRouteRecoveryCode::SessionRouteSelectionRequired
    ));

    let stale_catalog_error = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "must not allocate with stale catalog binding".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: Some(replacement),
                model_selection_binding: Some("stale-selection-binding".to_owned()),
                route_recovery_binding: Some(route_recovery_binding),
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect_err("stale model selection binding must reject synchronously");
    assert!(matches!(
        stale_catalog_error,
        crate::HttpRegistryError::SessionRunRecoveryRequired { recovery }
            if recovery.code
                == crate::HttpSessionRouteRecoveryCode::SessionRouteSelectionRequired
    ));
    assert!(
        registry
            .get_session(&session.id)
            .expect("session should remain registered")
            .run_ids
            .is_empty()
    );
}

#[tokio::test]
async fn production_supervisor_routes_typed_task_continuation_to_shared_preparer() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

{storage}

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = {{ source = "none" }}

[task]
enabled = true
"#
        ),
    )
    .expect("Task continuation config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-task.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures-task.json"), 16)
            .expect("disclosure journal should initialize"),
    );
    let preparer = Arc::new(FailingTaskPreparation::default());
    let driver = Arc::new(
        HttpProductionRunDriver::new_with_preparer(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
            preparer.clone(),
        )
        .expect("production driver should initialize"),
    );
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-task.json"), 16)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");

    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: String::new(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: Some(HttpTaskContinuationRequest {
                    task_id: "task-http-continuation".to_owned(),
                    guidance: Some("finish the HTTP control surface".to_owned()),
                }),
            },
        )
        .expect("typed Task continuation should start");
    let terminal = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = registry
                .get_run(&run.id)
                .expect("run should remain visible");
            if snapshot.status.is_terminal() {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Task preparation failure should reach a terminal");

    assert_eq!(terminal.status, HttpRunStatus::Failed);
    let requests = preparer
        .requests
        .lock()
        .expect("Task request recorder should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].task_id.as_str(), "task-http-continuation");
    assert_eq!(
        requests[0].guidance.as_deref(),
        Some("finish the HTTP control surface")
    );
    assert_eq!(
        requests[0].expected_session_scope_id,
        session.durable_session_scope_id
    );
}

fn production_queue_session_named(temp: &tempfile::TempDir, name: &str) -> HttpSessionSnapshot {
    let session_path = temp.path().join(format!("{name}.jsonl"));
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)
        .expect("queue session store should initialize");
    let (provider_name, route) = production_test_model_route(temp);
    let mut session = sigil_kernel::Session::new_with_route(provider_name, route).with_store(store);
    session
        .ensure_identity_entry()
        .expect("queue session identity should append");
    let durable_session_scope_id = session.session_scope_id().to_owned();
    drop(session);
    HttpSessionSnapshot {
        id: format!("adapter-{name}"),
        label: None,
        run_ids: Vec::new(),
        durable_session_scope_id,
        session_log_path: session_path.display().to_string(),
        foreground_run_id: None,
        route_transition: None,
        route_recovery: None,
    }
}

fn production_authority_session(
    driver: &HttpProductionRunDriver,
    _temp: &tempfile::TempDir,
    name: &str,
) -> HttpSessionSnapshot {
    let composition = driver
        .services
        .authority_composition()
        .expect("current-schema authority composition should be present");
    let lease = composition
        .storage_writer
        .acquire_named(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
            name,
        )
        .expect("authority session namespace should be admitted");
    let session_path = lease.path().join("records.jsonl");
    let store =
        JsonlSessionStore::new(&session_path).expect("authority session store should initialize");
    let (provider_name, route) = production_test_model_route_from_driver(driver);
    let mut session = sigil_kernel::Session::new_with_route(provider_name, route).with_store(store);
    session
        .ensure_identity_entry()
        .expect("authority session identity should append");
    let durable_session_scope_id = session.session_scope_id().to_owned();
    drop(session);
    drop(lease);
    HttpSessionSnapshot {
        id: format!("adapter-{name}"),
        label: None,
        run_ids: Vec::new(),
        durable_session_scope_id,
        session_log_path: session_path.display().to_string(),
        foreground_run_id: None,
        route_transition: None,
        route_recovery: None,
    }
}

fn production_test_model_route_from_driver(
    driver: &HttpProductionRunDriver,
) -> (String, sigil_kernel::ResolvedModelRoute) {
    let config = sigil_kernel::RootConfig::load(&driver.options.config_path)
        .expect("production test config should load");
    sigil_runtime::provider_connections::resolve_default_model_route(&config)
        .expect("V2 model route should resolve")
}

fn authority_artifact_store_root(
    driver: &HttpProductionRunDriver,
    session: &HttpSessionSnapshot,
) -> std::path::PathBuf {
    let key = authority_artifact_store_key(&driver.services, session)
        .expect("current-schema artifact authority should resolve the session key");
    driver
        .services
        .authority_composition()
        .expect("current-schema authority composition should be present")
        .storage_writer
        .managed_named_leaf_path(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore,
            &key,
        )
        .expect("artifact store namespace should resolve")
}

fn append_durable_tool_artifact(
    driver: &HttpProductionRunDriver,
    session: &HttpSessionSnapshot,
    result: ToolResult,
) -> ToolArtifactDescriptorV1 {
    let session_store = JsonlSessionStore::new(std::path::Path::new(&session.session_log_path))
        .expect("session store should reopen");
    let (recorded, _display) = with_authority_artifact_store(driver, session, |artifact_store| {
        ToolResultRecordedV3::capture(
            &result,
            Some(artifact_store),
            ToolArtifactSensitivity::Ordinary,
        )
        .expect("tool result should capture")
    });
    let descriptor = recorded
        .artifact
        .descriptor()
        .expect("tool result should publish an artifact")
        .clone();
    session_store
        .append(&SessionLogEntry::ToolResultV3(recorded))
        .expect("tool result should append durably");
    descriptor
}

fn with_authority_artifact_store<T>(
    driver: &HttpProductionRunDriver,
    session: &HttpSessionSnapshot,
    operation: impl FnOnce(&ToolArtifactStore) -> T,
) -> T {
    let lease = authority_artifact_store_for_session(&driver.services, session)
        .expect("current-schema artifact authority should admit the test session");
    let store = lease.store();
    let output = operation(&store);
    drop(lease);
    output
}

#[tokio::test]
async fn production_driver_authorizes_artifact_reads_by_exact_session_and_hash() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "artifact-read");
    let session = production_authority_session(&driver, &temp, "artifact-owner");
    let descriptor = append_durable_tool_artifact(
        &driver,
        &session,
        ToolResult::ok(
            "call-artifact",
            "shell",
            "line one\nline two\nline three\n",
            ToolResultMeta::default(),
        ),
    );
    assert!(
        with_authority_artifact_store(&driver, &session, |store| store
            .source_event_id(&descriptor.artifact_ref))
        .is_err(),
        "retrieval authority must not require a post-append sidecar"
    );
    let request = crate::HttpToolArtifactReadRequest {
        artifact_ref: descriptor.artifact_ref.artifact_id.clone(),
        selector: crate::HttpToolArtifactSelector::LinePage {
            start_line: 1,
            line_count: 2,
        },
    };

    let page = driver
        .tool_artifact_page(&session, &request)
        .expect("session owner should read a bounded page");
    assert_eq!(page.request_scope, session.id);
    assert_eq!(page.body, "line two\nline three\n");
    assert_eq!(page.returned_bytes, 20);
    assert_eq!(page.artifact_sha256, descriptor.content_sha256);
    let metrics_after_first_page = driver
        .session_projection_stores
        .lock()
        .expect("projection store cache should not be poisoned")
        .entries
        .get(&session.durable_session_scope_id)
        .expect("artifact read should retain the session projection store")
        .store
        .active_projection_metrics();
    driver
        .tool_artifact_page(&session, &request)
        .expect("a second page read should reuse the active projection");
    let metrics_after_second_page = driver
        .session_projection_stores
        .lock()
        .expect("projection store cache should not be poisoned")
        .entries
        .get(&session.durable_session_scope_id)
        .expect("artifact read should retain the session projection store")
        .store
        .active_projection_metrics();
    assert_eq!(
        metrics_after_second_page.full_rebuild_total, metrics_after_first_page.full_rebuild_total,
        "steady-state typed retrieval must not rescan the durable session"
    );

    let other_session = production_authority_session(&driver, &temp, "artifact-other");
    assert_eq!(
        driver.tool_artifact_page(&other_session, &request),
        Err(crate::HttpToolArtifactReadDriverError::Unavailable)
    );

    let binary_descriptor = with_authority_artifact_store(&driver, &session, |store| {
        store
            .capture_policy_safe_bytes(
                "call-binary",
                "read_binary",
                &[0xff, 0x00],
                2,
                "application/octet-stream",
                ToolArtifactEncoding::Binary,
                ToolArtifactSensitivity::Ordinary,
                0,
            )
            .expect("binary artifact should publish")
    });
    let binary_descriptor = append_durable_tool_artifact(
        &driver,
        &session,
        ToolResult::ok(
            "call-binary",
            "read_binary",
            "binary output",
            ToolResultMeta::default(),
        )
        .with_captured_artifact(binary_descriptor),
    );
    assert_eq!(
        driver.tool_artifact_page(
            &session,
            &crate::HttpToolArtifactReadRequest {
                artifact_ref: binary_descriptor.artifact_ref.artifact_id,
                selector: crate::HttpToolArtifactSelector::LinePage {
                    start_line: 0,
                    line_count: 1,
                },
            }
        ),
        Err(crate::HttpToolArtifactReadDriverError::InvalidSelector)
    );

    let long_line = "x".repeat(sigil_kernel::session::TOOL_ARTIFACT_READ_MAX_BYTES as usize + 128);
    let long_line_descriptor = append_durable_tool_artifact(
        &driver,
        &session,
        ToolResult::ok(
            "call-long-line",
            "shell",
            long_line,
            ToolResultMeta::default(),
        ),
    );
    let long_line_request = crate::HttpToolArtifactReadRequest {
        artifact_ref: long_line_descriptor.artifact_ref.artifact_id,
        selector: crate::HttpToolArtifactSelector::LinePage {
            start_line: 0,
            line_count: 1,
        },
    };
    let long_line_page = driver
        .tool_artifact_page(&session, &long_line_request)
        .expect("line paging should fall through to a bounded byte continuation");
    assert!(matches!(
        &long_line_page.next_selector,
        Some(crate::HttpToolArtifactSelector::ByteSlice { .. })
    ));
    long_line_page
        .validate(&session.id, &long_line_request)
        .expect("transport validation should accept the typed byte continuation");

    let digest = descriptor
        .content_sha256
        .strip_prefix("sha256:")
        .expect("descriptor hash prefix");
    let artifact_store_root = authority_artifact_store_root(&driver, &session);
    let blob_path = artifact_store_root
        .join("blobs")
        .join(&digest[..2])
        .join(format!("{digest}.blob"));
    std::fs::write(&blob_path, b"tampered")
        .expect("test should replace the session-owned artifact bytes");
    assert_eq!(
        driver.tool_artifact_page(&session, &request),
        Err(crate::HttpToolArtifactReadDriverError::Corrupt)
    );

    let oversized = crate::HttpToolArtifactReadRequest {
        artifact_ref: descriptor.artifact_ref.artifact_id,
        selector: crate::HttpToolArtifactSelector::ByteSlice {
            offset: 0,
            limit: sigil_kernel::session::TOOL_ARTIFACT_READ_MAX_BYTES + 1,
        },
    };
    assert_eq!(
        driver.tool_artifact_page(&session, &oversized),
        Err(crate::HttpToolArtifactReadDriverError::InvalidSelector)
    );
}

#[tokio::test]
async fn production_driver_uses_durable_projection_instead_of_forgeable_artifact_sidecars() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "artifact-binding");
    let session = production_authority_session(&driver, &temp, "artifact-binding-owner");

    let orphan = with_authority_artifact_store(&driver, &session, |store| {
        let orphan = store
            .capture_text(
                "call-orphan",
                "shell",
                "not durably bound",
                ToolArtifactSensitivity::Ordinary,
            )
            .expect("orphan artifact should publish");
        store
            .bind_source_event(&orphan.artifact_ref, "forged-source-event")
            .expect("orphan sidecar should be forgeable for the regression fixture");
        orphan
    });
    assert_eq!(
        driver.tool_artifact_page(
            &session,
            &crate::HttpToolArtifactReadRequest {
                artifact_ref: orphan.artifact_ref.artifact_id,
                selector: crate::HttpToolArtifactSelector::ByteSlice {
                    offset: 0,
                    limit: 64,
                },
            }
        ),
        Err(crate::HttpToolArtifactReadDriverError::Unavailable),
        "a sidecar without a durable ToolResultV2 must never authorize retrieval"
    );

    let descriptor = append_durable_tool_artifact(
        &driver,
        &session,
        ToolResult::ok(
            "call-bound",
            "shell",
            "durably projected",
            ToolResultMeta::default(),
        ),
    );
    let mut forged_descriptor = descriptor.clone();
    forged_descriptor.tool_name = "forged-tool".to_owned();
    let artifact_store_root = authority_artifact_store_root(&driver, &session);
    let manifest_path = artifact_store_root
        .join("refs")
        .join(format!("{}.json", descriptor.artifact_ref.artifact_id));
    std::fs::write(
        manifest_path,
        serde_json::to_vec(&forged_descriptor).expect("forged descriptor should encode"),
    )
    .expect("test should replace the descriptor manifest");
    with_authority_artifact_store(&driver, &session, |store| {
        assert_eq!(
            store
                .resolve(&descriptor.artifact_ref)
                .expect("forged but well-formed physical descriptor should resolve")
                .tool_name,
            "forged-tool"
        );
    });
    let projected_binding = driver
        .retained_session_projection_store(&session)
        .expect("projection store should remain available")
        .active_projection_snapshot()
        .expect("active projection should remain valid")
        .tool_output_pressure()
        .artifact_source_binding(&descriptor.artifact_ref)
        .expect("durable binding should remain authoritative");
    assert_eq!(projected_binding.tool_name, "shell");
    assert_eq!(
        driver.tool_artifact_page(
            &session,
            &crate::HttpToolArtifactReadRequest {
                artifact_ref: descriptor.artifact_ref.artifact_id,
                selector: crate::HttpToolArtifactSelector::ByteSlice {
                    offset: 0,
                    limit: 64,
                },
            }
        ),
        Err(crate::HttpToolArtifactReadDriverError::Corrupt),
        "a physical manifest that disagrees with durable projection must fail closed"
    );
}

#[tokio::test]
async fn production_driver_bounds_retained_session_projections_with_scope_safe_lru() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "projection-cache");
    let snapshot = |index: usize, path_suffix: &str| HttpSessionSnapshot {
        id: format!("adapter-cache-{index}"),
        label: None,
        run_ids: Vec::new(),
        durable_session_scope_id: format!("scope-cache-{index}"),
        session_log_path: temp
            .path()
            .join(format!("projection-cache-{index}-{path_suffix}.jsonl"))
            .display()
            .to_string(),
        foreground_run_id: None,
        route_transition: None,
        route_recovery: None,
    };

    for index in 0..MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES {
        driver
            .retained_session_projection_store(&snapshot(index, "original"))
            .expect("bounded projection store should cache");
    }
    let first = snapshot(0, "original");
    driver
        .retained_session_projection_store(&first)
        .expect("touching the first scope should refresh its LRU position");
    let overflow = snapshot(MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES, "original");
    driver
        .retained_session_projection_store(&overflow)
        .expect("one overflow scope should evict the coldest store");

    {
        let stores = driver
            .session_projection_stores
            .lock()
            .expect("projection store cache should lock");
        assert_eq!(
            stores.entries.len(),
            MAX_HTTP_RETAINED_SESSION_PROJECTION_STORES
        );
        assert!(stores.entries.contains_key(&first.durable_session_scope_id));
        assert!(
            !stores.entries.contains_key("scope-cache-1"),
            "the least recently used cold scope should be evicted"
        );
        assert!(
            stores
                .entries
                .contains_key(&overflow.durable_session_scope_id)
        );
    }

    let conflicting_path = snapshot(0, "conflicting");
    assert!(matches!(
        driver.retained_session_projection_store(&conflicting_path),
        Err(crate::HttpToolArtifactReadDriverError::Unavailable)
    ));
    let stores = driver
        .session_projection_stores
        .lock()
        .expect("projection store cache should relock");
    assert_eq!(
        stores
            .entries
            .get(&first.durable_session_scope_id)
            .expect("original scope binding should remain")
            .session_log_path,
        first.session_log_path,
        "the same durable scope must never switch to another physical path"
    );
}

fn queue_command(
    command_id: &str,
    expected_generation: crate::HttpConversationQueueGeneration,
    action: HttpConversationQueueCommandAction,
) -> HttpConversationQueueDriverCommand {
    HttpConversationQueueDriverCommand {
        command_id: command_id.to_owned(),
        client_id: "desktop-client-1".to_owned(),
        request: HttpConversationQueueCommandRequest {
            expected_generation,
            action,
        },
    }
}

fn queued_terminal_context(queued: &HttpQueuedRunPreparation) -> HttpQueuedRunTerminalContext {
    HttpQueuedRunTerminalContext {
        queue_id: queued.promotion.queue_id.clone(),
        dispatch_run_id: queued.promotion.dispatch_run_id.clone(),
        expected_queue_revision: queued.promotion.expected_queue_revision.clone(),
        prompt_hash: queued.promotion.prompt_hash.clone(),
        exact_prompt_key: queued.exact_prompt_key.clone(),
    }
}

fn production_queued_preparation(
    driver: &HttpProductionRunDriver,
    session: &mut HttpSessionSnapshot,
    admission: crate::HttpQueuedRunAdmission,
) -> HttpQueuedRunPreparation {
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approvals: Vec::new(),
                approval_lifecycles: Vec::new(),
                terminal_tasks: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should revalidate admission");
    queued
}

#[async_trait]
impl HttpApplicationRunPreparer for ControlledPreparation {
    async fn prepare(
        &self,
        _request: ApplicationRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .map_err(|_| anyhow!("controlled preparation release closed"))?
            .forget();
        Err(anyhow!(
            "controlled preparation released after cancellation"
        ))
    }

    async fn prepare_queued(
        &self,
        _request: ApplicationQueuedRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .map_err(|_| anyhow!("controlled queued preparation release closed"))?
            .forget();
        Err(anyhow!(
            "controlled queued preparation released after cancellation"
        ))
    }
}

#[async_trait]
impl HttpApplicationRunPreparer for FailingQueuedPreparation {
    async fn prepare(
        &self,
        _request: ApplicationRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        Err(anyhow!("ordinary preparation is not expected in this test"))
    }

    async fn prepare_queued(
        &self,
        _request: ApplicationQueuedRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        self.queued_calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow!("controlled queued preparation failure"))
    }
}

#[async_trait]
impl HttpApplicationRunPreparer for FailingTaskPreparation {
    async fn prepare(
        &self,
        _request: ApplicationRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        Err(anyhow!("ordinary preparation is not expected in this test"))
    }

    async fn prepare_queued(
        &self,
        _request: ApplicationQueuedRunRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        Err(anyhow!("queued preparation is not expected in this test"))
    }

    async fn prepare_task(
        &self,
        request: ApplicationTaskContinuationRequest,
        _services: ApplicationRunServices,
    ) -> Result<PreparedApplicationTaskContinuation> {
        self.requests
            .lock()
            .expect("Task request recorder should not be poisoned")
            .push(request);
        Err(anyhow!("controlled Task continuation preparation failure"))
    }
}

#[tokio::test]
async fn production_queue_mutations_are_durable_cas_guarded_and_owner_exact() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "cas");
    let session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    assert_eq!(initial.total_items, 0);

    let queued = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-safe-1",
                initial.generation.clone(),
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect Cargo.toml".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("safe prompt should enqueue");
    assert_eq!(queued.total_items, 1);
    assert_ne!(queued.generation, initial.generation);
    assert_eq!(
        queued.items[0].prompt_material,
        HttpConversationQueuePromptMaterial::PersistedSafe
    );
    assert!(queued.items[0].dispatchable);
    assert_eq!(
        queued.next_dispatchable_entry_id.as_deref(),
        Some(queued.items[0].entry_id.as_str())
    );
    assert_eq!(
        queued.items[0].entry_id,
        stable_http_queue_id(
            &session.durable_session_scope_id,
            "desktop-client-1",
            "enqueue-safe-1"
        )
        .expect("stable queue id should derive")
        .as_str()
    );

    let queued = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-safe-2",
                queued.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "then inspect README.md".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("second safe prompt should enqueue");
    assert_eq!(queued.total_items, 2);
    assert_eq!(
        queued.items[1].blocked_reason,
        Some(HttpConversationQueueBlockedReason::WaitingForTerminalFrontier)
    );
    assert!(!queued.items[1].dispatchable);

    let admission = driver
        .next_queued_run_admission(&session)
        .expect("queue admission should project")
        .expect("safe queued prompt should dispatch");
    assert_eq!(admission.entry_id, queued.items[0].entry_id);
    assert_eq!(admission.generation, queued.generation);
    assert_eq!(admission.prompt_preview, "inspect Cargo.toml");
    assert_eq!(
        driver
            .next_queued_run_admission(&session)
            .expect("repeat admission should project")
            .expect("repeat admission should remain available")
            .dispatch_run_id,
        admission.dispatch_run_id
    );

    let durable_after_enqueue =
        std::fs::read(&session.session_log_path).expect("queue session should read");
    assert_eq!(
        driver.mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "stale-pause",
                initial.generation,
                HttpConversationQueueCommandAction::Pause,
            ),
        ),
        Err(HttpConversationQueueDriverError::StaleGeneration)
    );
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("queue session should reread"),
        durable_after_enqueue
    );

    let owner = HttpForegroundRunOwner {
        run_id: "run-active".to_owned(),
        owner_revision: "owner-revision-1".to_owned(),
    };
    let wrong_owner = HttpForegroundRunOwner {
        run_id: owner.run_id.clone(),
        owner_revision: "owner-revision-stale".to_owned(),
    };
    let interrupt = queue_command(
        "interrupt-next",
        queued.generation.clone(),
        HttpConversationQueueCommandAction::InterruptAndRunNext {
            foreground_run_id: owner.run_id.clone(),
            foreground_owner_revision: owner.owner_revision.clone(),
        },
    );
    assert_eq!(
        driver.mutate_conversation_queue(&session, Some(&wrong_owner), &interrupt),
        Err(HttpConversationQueueDriverError::OwnerLost)
    );
    let owner_view = driver
        .mutate_conversation_queue(&session, Some(&owner), &interrupt)
        .expect("exact owner interrupt guard should be accepted without a durable mutation");
    assert_eq!(owner_view.generation, queued.generation);
    assert!(!owner_view.items[0].dispatchable);
    assert_eq!(
        owner_view.items[0].blocked_reason,
        Some(HttpConversationQueueBlockedReason::ForegroundRunActive)
    );
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("queue session should reread"),
        durable_after_enqueue
    );
}

#[tokio::test]
async fn production_queue_interrupt_requires_one_exact_dispatchable_next_item() {
    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-empty");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("empty queue should project");
        let owner = HttpForegroundRunOwner {
            run_id: "run-empty".to_owned(),
            owner_revision: "owner-empty".to_owned(),
        };
        let durable_before = std::fs::read(&session.session_log_path)
            .expect("empty queue durable stream should read");

        assert_eq!(
            driver.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-empty",
                    initial.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::Conflict)
        );
        assert_eq!(
            std::fs::read(&session.session_log_path)
                .expect("empty queue durable stream should reread"),
            durable_before
        );
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-unsupported");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("unsupported queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-plan-prompt",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "plan the next change".to_owned(),
                        kind: HttpConversationQueueItemKind::PlanPrompt,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("unsupported item should remain visible in the queue");
        let owner = HttpForegroundRunOwner {
            run_id: "run-unsupported".to_owned(),
            owner_revision: "owner-unsupported".to_owned(),
        };

        assert_eq!(
            driver.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-unsupported",
                    queued.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::Unsupported)
        );
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-paused");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("paused queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-paused-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("paused candidate should enqueue");
        let paused = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-pause",
                    queued.generation,
                    HttpConversationQueueCommandAction::Pause,
                ),
            )
            .expect("queue should pause");
        let owner = HttpForegroundRunOwner {
            run_id: "run-paused".to_owned(),
            owner_revision: "owner-paused".to_owned(),
        };

        assert_eq!(
            driver.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-paused",
                    paused.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::Conflict)
        );
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "interrupt-reentry-before-restart");
        let session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("reentry queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "interrupt-reentry-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect with authorization=process-local-secret".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("exact prompt should enqueue");
        drop(driver);
        let restarted = production_queue_driver(&temp, "interrupt-reentry-after-restart");
        let owner = HttpForegroundRunOwner {
            run_id: "run-reentry".to_owned(),
            owner_revision: "owner-reentry".to_owned(),
        };

        assert_eq!(
            restarted.mutate_conversation_queue(
                &session,
                Some(&owner),
                &queue_command(
                    "interrupt-reentry",
                    queued.generation,
                    HttpConversationQueueCommandAction::InterruptAndRunNext {
                        foreground_run_id: owner.run_id.clone(),
                        foreground_owner_revision: owner.owner_revision.clone(),
                    },
                ),
            ),
            Err(HttpConversationQueueDriverError::RequiresReentry)
        );
    }
}

#[test]
fn production_queue_stable_identity_seed_has_no_delimiter_tuple_collision() {
    let left = stable_http_queue_id("scope:a", "client", "command")
        .expect("left stable queue id should derive");
    let right = stable_http_queue_id("scope", "a:client", "command")
        .expect("right stable queue id should derive");

    assert_ne!(left, right);
    assert_ne!(
        stable_http_identity_seed(&["scope:a", "client", "command"]),
        stable_http_identity_seed(&["scope", "a:client", "command"])
    );
}

#[tokio::test]
async fn production_queue_exact_prompt_is_process_local_and_requires_reentry_after_restart() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "exact-owner-1");
    let session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    let raw_prompt = "inspect with authorization=super-secret-value";
    let queued = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-exact-1",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: raw_prompt.to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("exact prompt should enqueue with process-local material");
    assert_eq!(
        queued.items[0].prompt_material,
        HttpConversationQueuePromptMaterial::AvailableProcessLocal
    );
    assert!(queued.items[0].dispatchable);
    assert!(
        driver
            .next_queued_run_admission(&session)
            .expect("queue admission should project")
            .is_some()
    );
    let durable =
        std::fs::read_to_string(&session.session_log_path).expect("queue session should read");
    assert!(!durable.contains(raw_prompt));
    assert!(!durable.contains("super-secret-value"));

    drop(driver);
    let restarted = production_queue_driver(&temp, "exact-owner-2");
    let restarted_view = restarted
        .conversation_queue_view(&session, None)
        .expect("restarted owner should project durable queue");
    assert_eq!(
        restarted_view.items[0].prompt_material,
        HttpConversationQueuePromptMaterial::RequiresReentry
    );
    assert_eq!(
        restarted_view.items[0].blocked_reason,
        Some(HttpConversationQueueBlockedReason::RequiresReentry)
    );
    assert!(!restarted_view.items[0].dispatchable);
    assert!(restarted_view.next_dispatchable_entry_id.is_none());
    assert!(
        restarted
            .next_queued_run_admission(&session)
            .expect("restarted admission should project")
            .is_none()
    );
}

#[tokio::test]
async fn production_queue_restart_rebinds_persisted_reasoning_effort_before_dispatch() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "effort-owner-1");
    write_reasoning_test_config(&temp.path().join("sigil.toml"));
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-effort-before-restart",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "reply with the durable queue effort".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: Some(crate::HttpReasoningEffort::Low),
                },
            ),
        )
        .expect("reasoning effort should persist with the safe queue item");
    drop(driver);

    let restarted = production_queue_driver(&temp, "effort-owner-2");
    write_reasoning_test_config(&temp.path().join("sigil.toml"));
    let admission = restarted
        .next_queued_run_admission(&session)
        .expect("restarted admission should project")
        .expect("persisted safe prompt should remain dispatchable");
    assert_eq!(
        admission.reasoning_effort,
        Some(crate::HttpReasoningEffort::Low)
    );
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (start, _) = restarted
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approvals: Vec::new(),
                approval_lifecycles: Vec::new(),
                terminal_tasks: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("restarted queued dispatch should reconstruct current capability bindings");
    let current_context = application_run_context_view(
        &restarted.options.config_path,
        &restarted.options.launch_cwd,
        Path::new(&session.session_log_path),
        &session.durable_session_scope_id,
    )
    .expect("current run context should project");

    assert_eq!(
        start.reasoning_effort_binding,
        current_context.reasoning_effort_binding
    );
    assert!(start.reasoning_effort_binding.is_some());
}

#[tokio::test]
async fn production_queue_session_delete_purges_only_matching_exact_prompt_material() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "delete-purge");
    let first_session = production_queue_session_named(&temp, "delete-first");
    let second_session = production_queue_session_named(&temp, "delete-second");
    for (session, command_id, secret) in [
        (&first_session, "delete-first-exact", "first-delete-secret"),
        (
            &second_session,
            "delete-second-exact",
            "second-delete-secret",
        ),
    ] {
        let initial = driver
            .conversation_queue_view(session, None)
            .expect("delete purge initial queue should project");
        driver
            .mutate_conversation_queue(
                session,
                None,
                &queue_command(
                    command_id,
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: format!("inspect with authorization={secret}"),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("delete purge exact prompt should enqueue");
    }
    assert_eq!(
        driver
            .exact_queue_prompts
            .lock()
            .expect("delete purge cache should lock")
            .len(),
        2
    );
    driver
        .retained_session_projection_store(&first_session)
        .expect("first projection store should cache");
    driver
        .retained_session_projection_store(&second_session)
        .expect("second projection store should cache");
    assert_eq!(
        driver
            .session_projection_stores
            .lock()
            .expect("projection store cache should lock")
            .entries
            .len(),
        2
    );

    driver.purge_session_local_state(&first_session.durable_session_scope_id);

    let exact_prompts = driver
        .exact_queue_prompts
        .lock()
        .expect("delete purge cache should relock");
    assert_eq!(exact_prompts.len(), 1);
    assert!(
        exact_prompts
            .keys()
            .all(|key| key.session_scope_id == second_session.durable_session_scope_id)
    );
    let session_projection_stores = driver
        .session_projection_stores
        .lock()
        .expect("projection store cache should relock");
    assert_eq!(session_projection_stores.entries.len(), 1);
    assert!(
        session_projection_stores
            .entries
            .contains_key(&second_session.durable_session_scope_id),
        "session deletion must release only the matching active projection handle"
    );
}

#[tokio::test]
async fn production_queue_unpromoted_terminal_consumes_only_the_admitted_item() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "unpromoted-terminal");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    let first = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-first",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect with authorization=first-secret".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("first prompt should enqueue");
    let queue = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-second",
                first.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect README.md next".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("second prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("admission should project")
        .expect("first prompt should admit");
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approvals: Vec::new(),
                approval_lifecycles: Vec::new(),
                terminal_tasks: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should revalidate admission");
    let terminal = queued_terminal_context(&queued);

    finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
        .expect("preparation failure should consume the admitted item");
    evict_http_promoted_exact_prompt(&session, Some(&terminal), &driver.exact_queue_prompts)
        .expect("terminal item should evict process-local exact material");
    let projected = driver
        .conversation_queue_view(&session, None)
        .expect("terminal queue should project");
    assert_eq!(projected.total_items, 1);
    assert_eq!(
        projected.items[0].status,
        crate::HttpConversationQueueItemStatus::Queued
    );
    assert_eq!(
        projected.next_dispatchable_entry_id.as_deref(),
        Some(projected.items[0].entry_id.as_str())
    );
    assert_ne!(projected.items[0].entry_id, terminal.queue_id.as_str());
    assert_eq!(
        driver.mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "edit-terminal-item",
                projected.generation.clone(),
                HttpConversationQueueCommandAction::Edit {
                    entry_id: terminal.queue_id.as_str().to_owned(),
                    prompt: "must not revive a terminal item".to_owned(),
                    reasoning_effort: None,
                },
            ),
        ),
        Err(HttpConversationQueueDriverError::Terminal)
    );
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("terminal queue entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Rejected
            ))
    );
    assert!(
        !driver
            .exact_queue_prompts
            .lock()
            .expect("exact prompt cache should lock")
            .contains_key(&terminal.exact_prompt_key)
    );
    let next = driver
        .next_queued_run_admission(&session)
        .expect("next admission should project")
        .expect("unconsumed second item should remain dispatchable");
    assert_eq!(next.entry_id, queue.items[1].entry_id);
    assert_ne!(next.dispatch_run_id, terminal.dispatch_run_id);
}

#[tokio::test]
async fn production_queue_unpromoted_terminal_does_not_consume_mutation_drift() {
    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "terminal-edit-drift");
        let mut session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("edit drift initial queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "edit-drift-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("edit drift prompt should enqueue");
        let admission = driver
            .next_queued_run_admission(&session)
            .expect("edit drift admission should project")
            .expect("edit drift prompt should admit");
        let original_dispatch_run_id = admission.dispatch_run_id.clone();
        let preparation = production_queued_preparation(&driver, &mut session, admission);
        let terminal = queued_terminal_context(&preparation);
        let edited = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "edit-drift-mutation",
                    queued.generation,
                    HttpConversationQueueCommandAction::Edit {
                        entry_id: terminal.queue_id.as_str().to_owned(),
                        prompt: "inspect the workspace manifest instead".to_owned(),
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("queued edit should win its durable CAS");
        finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
            .expect("stale preparation terminal should become a zero-write race outcome");
        session.foreground_run_id = None;
        let projected = driver
            .conversation_queue_view(&session, None)
            .expect("edited queue should remain active");
        assert_eq!(projected.generation, edited.generation);
        assert_eq!(projected.total_items, 1);
        assert!(
            projected.items[0]
                .prompt_preview
                .contains("workspace manifest")
        );
        assert!(
            JsonlSessionStore::read_entries(&session.session_log_path)
                .expect("edit drift entries should read")
                .iter()
                .all(|entry| !matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                        if status.queue_id == terminal.queue_id
                            && status.status == ConversationInputStatus::Rejected
                ))
        );
        let retry = driver
            .next_queued_run_admission(&session)
            .expect("edited prompt retry admission should project")
            .expect("edited prompt should remain dispatchable");
        assert_eq!(retry.entry_id, terminal.queue_id.as_str());
        assert_ne!(retry.dispatch_run_id, original_dispatch_run_id);
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "terminal-remove-drift");
        let mut session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("remove drift initial queue should project");
        let queued = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "remove-drift-enqueue",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("remove drift prompt should enqueue");
        let admission = driver
            .next_queued_run_admission(&session)
            .expect("remove drift admission should project")
            .expect("remove drift prompt should admit");
        let preparation = production_queued_preparation(&driver, &mut session, admission);
        let terminal = queued_terminal_context(&preparation);
        driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "remove-drift-mutation",
                    queued.generation,
                    HttpConversationQueueCommandAction::Remove {
                        entry_id: terminal.queue_id.as_str().to_owned(),
                    },
                ),
            )
            .expect("queued removal should win its durable CAS");
        finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
            .expect("removed queue item should not receive a second terminal");
        let statuses =
            JsonlSessionStore::read_entries(&session.session_log_path)
                .expect("remove drift entries should read")
                .into_iter()
                .filter_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(
                        status,
                    )) if status.queue_id == terminal.queue_id => Some(status.status),
                    _ => None,
                })
                .collect::<Vec<_>>();
        assert_eq!(statuses, vec![ConversationInputStatus::Cancelled]);
    }

    {
        let temp = tempfile::tempdir().expect("temporary directory should exist");
        let driver = production_queue_driver(&temp, "terminal-reorder-drift");
        let mut session = production_queue_session(&temp);
        let initial = driver
            .conversation_queue_view(&session, None)
            .expect("reorder drift initial queue should project");
        let first = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "reorder-drift-first",
                    initial.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect Cargo.toml first".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("reorder drift first prompt should enqueue");
        let second = driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "reorder-drift-second",
                    first.generation,
                    HttpConversationQueueCommandAction::Enqueue {
                        prompt: "inspect README.md second".to_owned(),
                        kind: HttpConversationQueueItemKind::Chat,
                        reasoning_effort: None,
                    },
                ),
            )
            .expect("reorder drift second prompt should enqueue");
        let admission = driver
            .next_queued_run_admission(&session)
            .expect("reorder drift admission should project")
            .expect("reorder drift first prompt should admit");
        let preparation = production_queued_preparation(&driver, &mut session, admission);
        let terminal = queued_terminal_context(&preparation);
        let second_entry_id = second.items[1].entry_id.clone();
        driver
            .mutate_conversation_queue(
                &session,
                None,
                &queue_command(
                    "reorder-drift-mutation",
                    second.generation,
                    HttpConversationQueueCommandAction::Reorder {
                        entry_id: terminal.queue_id.as_str().to_owned(),
                        after_entry_id: Some(second_entry_id.clone()),
                    },
                ),
            )
            .expect("queued reorder should win its durable CAS");
        finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
            .expect("reordered queue item should not receive a stale terminal");
        session.foreground_run_id = None;
        let retry = driver
            .next_queued_run_admission(&session)
            .expect("reordered queue admission should project")
            .expect("new FIFO head should remain dispatchable");
        assert_eq!(retry.entry_id, second_entry_id);
        assert!(
            JsonlSessionStore::read_entries(&session.session_log_path)
                .expect("reorder drift entries should read")
                .iter()
                .all(|entry| !matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                        if status.queue_id == terminal.queue_id
                            && status.status == ConversationInputStatus::Rejected
                ))
        );
    }
}

#[tokio::test]
async fn production_queue_cancel_before_promotion_is_terminal_without_replay() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "unpromoted-cancel");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    let queue = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-cancelled",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "cancel this queued run".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("admission should project")
        .expect("prompt should admit");
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approvals: Vec::new(),
                approval_lifecycles: Vec::new(),
                terminal_tasks: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should revalidate admission");
    let terminal = queued_terminal_context(&queued);

    finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Cancelled)
        .expect("pre-promotion cancellation should become terminal");
    let projected = driver
        .conversation_queue_view(&session, None)
        .expect("cancelled queue should project");
    assert_eq!(queue.items[0].entry_id, terminal.queue_id.as_str());
    assert_eq!(projected.total_items, 0);
    assert!(projected.items.is_empty());
    assert!(projected.next_dispatchable_entry_id.is_none());
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("cancelled queue entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Cancelled
            ))
    );
    assert!(
        driver
            .next_queued_run_admission(&session)
            .expect("post-cancel admission should project")
            .is_none()
    );
}

#[tokio::test]
async fn production_queue_promotion_evicts_exact_material_and_terminal_uses_attempt_evidence() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "promoted-terminal");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("initial queue should project");
    driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "enqueue-promoted",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect with authorization=promotion-secret".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("exact prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("admission should project")
        .expect("prompt should admit");
    session.foreground_run_id = Some(admission.dispatch_run_id.clone());
    let (_, queued) = driver
        .queued_supervisor_start(HttpQueuedRunDriverStart {
            session: session.clone(),
            run: crate::HttpRunSnapshot {
                id: admission.dispatch_run_id.clone(),
                session_id: session.id.clone(),
                status: HttpRunStatus::Starting,
                permission_mode: admission.permission_mode,
                reasoning_effort: admission.reasoning_effort,
                prompt_preview: admission.prompt_preview.clone(),
                pending_approvals: Vec::new(),
                approval_lifecycles: Vec::new(),
                terminal_tasks: Vec::new(),
                stream_sequence: 0,
            },
            admission,
        })
        .expect("queued supervisor preparation should materialize promotion input");
    let terminal = queued_terminal_context(&queued);
    let store = JsonlSessionStore::new(&session.session_log_path)
        .expect("queue session store should reopen");
    store
        .append_conversation_input_promoted(queued.promotion)
        .expect("promotion should commit under the durable queue CAS");

    evict_http_promoted_exact_prompt(&session, Some(&terminal), &driver.exact_queue_prompts)
        .expect("promotion should evict process-local exact material");
    assert!(
        driver
            .exact_queue_prompts
            .lock()
            .expect("exact prompt cache should lock")
            .is_empty()
    );
    finalize_http_queued_terminal(&session, &terminal, HttpQueuedUnpromotedTerminal::Rejected)
        .expect("missing physical attempt should reject promoted queue item");

    let entries = JsonlSessionStore::read_entries(&session.session_log_path)
        .expect("promoted queue entries should read");
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion))
            if promotion.queue_id == terminal.queue_id
                && promotion.dispatch_run_id == terminal.dispatch_run_id
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.queue_id == terminal.queue_id
                && status.status == ConversationInputStatus::Rejected
    )));
    assert!(
        entries
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_)))
    );
}

#[tokio::test]
async fn production_queue_restart_reconciles_orphan_dispatch_before_next_admission() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "orphan-owner-before-restart");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("orphan initial queue should project");
    let first = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "orphan-first",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect Cargo.toml first".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("orphan first prompt should enqueue");
    let second = driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "orphan-second",
                first.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect README.md second".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("orphan second prompt should enqueue");
    let second_entry_id = second.items[1].entry_id.clone();
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("orphan first admission should project")
        .expect("orphan first prompt should admit");
    let preparation = production_queued_preparation(&driver, &mut session, admission);
    let terminal = queued_terminal_context(&preparation);
    JsonlSessionStore::new(&session.session_log_path)
        .expect("orphan queue store should reopen")
        .append_conversation_input_promoted(preparation.promotion)
        .expect("orphan promotion should commit");
    session.foreground_run_id = None;
    drop(driver);

    let restarted = production_queue_driver(&temp, "orphan-owner-after-restart");
    let durable_before_get = std::fs::read(&session.session_log_path)
        .expect("orphan durable stream should read before GET");
    let blocked = restarted
        .conversation_queue_view(&session, None)
        .expect("orphan queue GET should remain a pure projection");
    assert_eq!(blocked.total_items, 2);
    assert_eq!(
        blocked.items[0].status,
        crate::HttpConversationQueueItemStatus::Dispatching
    );
    assert_eq!(
        blocked.items[0].blocked_reason,
        Some(HttpConversationQueueBlockedReason::Conflict)
    );
    assert!(!blocked.items[1].dispatchable);
    assert_eq!(
        blocked.items[1].blocked_reason,
        Some(HttpConversationQueueBlockedReason::WaitingForTerminalFrontier)
    );
    assert!(blocked.next_dispatchable_entry_id.is_none());
    assert_eq!(
        std::fs::read(&session.session_log_path)
            .expect("orphan durable stream should read after GET"),
        durable_before_get,
        "queue GET must not reconcile durable orphan state"
    );

    let next = restarted
        .next_queued_run_admission(&session)
        .expect("scheduler admission should reconcile orphan dispatch evidence")
        .expect("second prompt should admit after orphan terminal convergence");
    assert_eq!(next.entry_id, second_entry_id);
    let projected = restarted
        .conversation_queue_view(&session, None)
        .expect("reconciled queue should project");
    assert_eq!(projected.total_items, 1);
    assert_eq!(projected.items[0].entry_id, second_entry_id);
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("orphan terminal entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Rejected
            ))
    );
}

#[tokio::test]
async fn production_queue_orphan_reconciliation_retries_frontier_drift() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "orphan-frontier-drift");
    let mut session = production_queue_session(&temp);
    let initial = driver
        .conversation_queue_view(&session, None)
        .expect("frontier drift initial queue should project");
    driver
        .mutate_conversation_queue(
            &session,
            None,
            &queue_command(
                "frontier-drift-enqueue",
                initial.generation,
                HttpConversationQueueCommandAction::Enqueue {
                    prompt: "inspect Cargo.toml".to_owned(),
                    kind: HttpConversationQueueItemKind::Chat,
                    reasoning_effort: None,
                },
            ),
        )
        .expect("frontier drift prompt should enqueue");
    let admission = driver
        .next_queued_run_admission(&session)
        .expect("frontier drift admission should project")
        .expect("frontier drift prompt should admit");
    let preparation = production_queued_preparation(&driver, &mut session, admission);
    let terminal = queued_terminal_context(&preparation);
    JsonlSessionStore::new(&session.session_log_path)
        .expect("frontier drift store should reopen")
        .append_conversation_input_promoted(preparation.promotion)
        .expect("frontier drift promotion should commit");
    session.foreground_run_id = None;

    let mut injected_drift = false;
    driver
        .reconcile_orphaned_queued_dispatches_with(&session, |store| {
            if !injected_drift {
                injected_drift = true;
                store
                    .append(&SessionLogEntry::Assistant(ModelMessage::assistant(
                        Some(
                            "unrelated durable event advances the terminal evidence frontier"
                                .to_owned(),
                        ),
                        Vec::new(),
                    )))
                    .map_err(|_| HttpConversationQueueDriverError::Unavailable)?;
            }
            Ok(())
        })
        .expect("orphan reconciliation should retry one exact frontier drift");
    assert!(injected_drift);
    assert!(
        driver
            .conversation_queue_view(&session, None)
            .expect("frontier drift queue should converge")
            .items
            .is_empty()
    );
    assert!(
        JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("frontier drift terminal entries should read")
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
                    if status.queue_id == terminal.queue_id
                        && status.status == ConversationInputStatus::Rejected
            ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_queue_scheduler_uses_supervisor_and_terminalizes_preparation_failure_once() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("scheduler-protocol.json"), 16)
            .expect("queue scheduler protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("scheduler-disclosures.json"),
            16,
        )
        .expect("queue scheduler disclosure journal should initialize"),
    );
    let preparer = Arc::new(FailingQueuedPreparation::default());
    let driver = Arc::new(
        HttpProductionRunDriver::new_with_preparer(
            HttpProductionRunDriverOptions::new(config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
            preparer.clone(),
        )
        .expect("production queue scheduler driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("scheduler-commands.json"), 16)
            .expect("queue scheduler command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production queue scheduler registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("queue scheduler session should bind");
    let initial = registry
        .conversation_queue(&session.id)
        .expect("queue scheduler initial projection should read");
    let command = crate::HttpCommandEnvelope::new(
        "scheduler-enqueue-1",
        "desktop-client-1",
        &session.id,
        HttpConversationQueueCommandRequest {
            expected_generation: initial.generation,
            action: HttpConversationQueueCommandAction::Enqueue {
                prompt: "inspect Cargo.toml".to_owned(),
                kind: HttpConversationQueueItemKind::Chat,
                reasoning_effort: None,
            },
        },
    );
    let receipt = registry
        .command_conversation_queue(&session.id, command)
        .expect("queue scheduler enqueue should commit before admission");
    assert_eq!(receipt.queue.total_items, 1);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let queue = registry
                .conversation_queue(&session.id)
                .expect("queue scheduler terminal projection should read");
            if preparer.queued_calls.load(Ordering::SeqCst) == 1
                && queue.total_items == 0
                && driver
                    .active_run_count()
                    .expect("queue scheduler active runs should read")
                    == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued preparation failure should terminalize without a scheduler hot loop");
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(preparer.queued_calls.load(Ordering::SeqCst), 1);
    let run_ids = registry
        .get_session(&session.id)
        .expect("queue scheduler session should remain bound")
        .run_ids;
    assert_eq!(run_ids.len(), 1);
    assert_eq!(
        registry
            .get_run(&run_ids[0])
            .expect("queue scheduler failed run should remain inspectable")
            .status,
        HttpRunStatus::Failed
    );
}

#[test]
fn approval_broker_routes_one_explicit_decision_with_stable_guards() {
    let broker = Arc::new(HttpApprovalBroker::default());
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            &approval_identity(),
            false,
            unavailable_session_grant_reason(),
            crate::HttpPendingApprovalDisplay::default(),
        )
        .expect("approval should register");
    assert_eq!(pending.policy_version, "permission-policy-v2");
    assert_eq!(pending.approval_request_id, "kernel-approval-v2:request-1");
    assert_eq!(pending.tool_call_hash.len(), 64);

    broker
        .resolve(
            "call-1",
            &pending.approval_request_id,
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
                family_pattern: None,
            },
        )
        .expect("decision should resolve");
    let outcome = broker
        .wait_for_decision("call-1", &pending.approval_request_id)
        .expect("resolved wait should finish");

    assert!(matches!(
        outcome.decision,
        Some(HttpApprovalDecisionRecord {
            decision: ToolApprovalUserDecision::Approved,
            ..
        })
    ));
}

#[test]
fn approval_broker_keeps_an_idle_request_pending_until_an_explicit_decision() {
    let broker = HttpApprovalBroker::default();
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            &approval_identity(),
            false,
            unavailable_session_grant_reason(),
            crate::HttpPendingApprovalDisplay::default(),
        )
        .expect("approval should register");

    std::thread::sleep(Duration::from_millis(20));
    assert!(
        broker
            .pending
            .lock()
            .expect("broker should lock")
            .contains_key(&pending.approval_request_id)
    );
    broker
        .resolve(
            "call-1",
            &pending.approval_request_id,
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Denied,
                reason: Some("explicit denial".to_owned()),
                family_pattern: None,
            },
        )
        .expect("explicit decision should resolve");
    let outcome = broker
        .wait_for_decision("call-1", &pending.approval_request_id)
        .expect("explicit decision should end the wait");
    assert!(matches!(
        outcome.decision,
        Some(HttpApprovalDecisionRecord {
            decision: ToolApprovalUserDecision::Denied,
            ..
        })
    ));
}

#[test]
fn approval_handler_only_resolves_explicit_broker_decisions() {
    let broker = Arc::new(HttpApprovalBroker::default());
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Write, None),
            &approval_identity(),
            false,
            unavailable_session_grant_reason(),
            crate::HttpPendingApprovalDisplay::default(),
        )
        .expect("approval should register");
    broker
        .resolve(
            "call-1",
            &pending.approval_request_id,
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::Approved,
                reason: None,
                family_pattern: None,
            },
        )
        .expect("decision should resolve");
    let mut handler = HttpProductionApprovalHandler {
        run_id: "run-1".to_owned(),
        broker,
    };

    assert!(matches!(
        handler
            .approve_tool_call_with_context(
                &call(),
                &spec(ToolAccess::Write, None),
                &approval_context(),
            )
            .expect("explicit decision should resolve"),
        ToolApproval::Approve
    ));
    assert!(handler.approval_is_explicit_user_action());
}

#[test]
fn approval_handler_preserves_bounded_session_decisions() {
    let broker = Arc::new(HttpApprovalBroker::default());
    let pending = broker
        .register(
            "run-1",
            &call(),
            &spec(ToolAccess::Read, None),
            &approval_identity(),
            true,
            None,
            crate::HttpPendingApprovalDisplay::default(),
        )
        .expect("approval should register");
    assert!(pending.session_grant_available);
    broker
        .resolve(
            "call-1",
            &pending.approval_request_id,
            HttpApprovalDecisionRecord {
                run_id: "run-1".to_owned(),
                call_id: "call-1".to_owned(),
                decision: ToolApprovalUserDecision::ApprovedForSession,
                reason: None,
                family_pattern: None,
            },
        )
        .expect("session decision should resolve");
    let mut handler = HttpProductionApprovalHandler {
        run_id: "run-1".to_owned(),
        broker,
    };

    assert!(matches!(
        handler
            .approve_tool_call_with_context(
                &call(),
                &spec(ToolAccess::Read, None),
                &approval_context(),
            )
            .expect("session decision should reach the kernel"),
        ToolApproval::ApproveForSession
    ));
}

#[test]
fn r71_headless_permission_fixture_matches_kernel_blockers() {
    let mut confirmation = PermissionDecision::new(
        ApprovalMode::Allow,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("notes.txt", "notes.txt")],
        false,
    );
    confirmation.confirmation = Some(PermissionConfirmation::TypePhrase {
        phrase: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
    });
    assert_eq!(
        confirmation.headless_blocker(),
        Some(sigil_kernel::HeadlessPermissionBlockerV1::ConfirmationRequired)
    );

    confirmation.confirmation = None;
    assert_eq!(confirmation.headless_blocker(), None);
    confirmation.mode = ApprovalMode::Ask;
    assert_eq!(
        confirmation.headless_blocker(),
        Some(sigil_kernel::HeadlessPermissionBlockerV1::ApprovalRequired)
    );
}

#[tokio::test]
async fn production_driver_rejects_an_in_memory_only_event_bus() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );

    assert!(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new("sigil.toml", "."),
            disclosure_journal,
            Arc::new(HttpLiveEventBus::new(8)),
            tokio::runtime::Handle::current(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn production_driver_session_reopen_revalidates_lifecycle_and_durable_truth() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let session_path = sessions.join("session-history.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)
        .expect("durable session store should open");
    let (provider_name, route) = production_test_model_route(&temp);
    let mut session = sigil_kernel::Session::new_with_route(provider_name, route).with_store(store);
    session
        .ensure_identity_entry()
        .expect("durable session identity should append");
    sigil_runtime::bind_session_composition(
        &mut session,
        &sigil_kernel::RootConfig::load(&config_path).expect("fixture config should load"),
    )
    .expect("current session contract should bind before execution history");
    session
        .append_user_message(sigil_kernel::ModelMessage::user("history"))
        .expect("durable message should append");
    session
        .append_assistant_message(sigil_kernel::ModelMessage::assistant_with_kind(
            Some("durable answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ))
        .expect("durable assistant should append");
    let task_id = TaskId::new("task-restart-control").expect("task id should be valid");
    let step_id = TaskStepId::new("inspect-code").expect("step id should be valid");
    let private_objective = "private restart objective with /private/worktree";
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")
                .expect("session ref should be valid"),
            objective: private_objective.to_owned(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }))
        .expect("task run should append");
    session
        .append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Inspect durable state".to_owned(),
                display_name: None,
                detail: Some("private planner detail".to_owned()),
                role: AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: None,
            }],
            reason: None,
        }))
        .expect("task plan should append");
    session
        .append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Interrupted,
            title: None,
            summary: Some("private participant summary".to_owned()),
            reason: None,
        }))
        .expect("task step should append");
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")
                .expect("session ref should be valid"),
            objective: private_objective.to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        }))
        .expect("paused task run should append");
    let durable_session_id = session.session_scope_id().to_owned();
    drop(session);
    bind_existing_application_session(&config_path, &session_path)
        .expect("V2 fixture session should bind directly before HTTP reopen");

    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        "workspace-1",
        &sessions,
        temp.path().join("exports"),
    );
    let options = HttpProductionRunDriverOptions::new(&config_path, temp.path())
        .with_session_lifecycle(lifecycle);
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            options,
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 8)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let request = HttpSessionOpenRequest {
        session_ref: "session-history.jsonl".to_owned(),
        session_id: durable_session_id.clone(),
        label: Some("History".to_owned()),
        recovery_binding: None,
    };
    let external_attachment =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_path,
        )
        .expect("external controller should own the fixture attachment");

    let opened = registry
        .open_session(request.clone())
        .expect("busy current durable source should still reopen read-only");

    assert_eq!(opened.durable_session_scope_id, durable_session_id);
    assert!(opened.route_transition.is_none());
    let attachment_recovery = opened
        .route_recovery
        .as_ref()
        .expect("busy read handle should carry exact attachment recovery");
    assert_eq!(
        attachment_recovery.code,
        crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive
    );
    let transcript = registry
        .transcript_page(&opened.id, None, 50)
        .expect("production transcript should project");
    assert_eq!(transcript.session_scope_id, durable_session_id);
    assert_eq!(transcript.total_messages, 2);
    assert_eq!(
        transcript.messages[1].content.as_deref(),
        Some("durable answer")
    );
    assert_eq!(
        transcript.messages[1].assistant_kind,
        Some(crate::HttpTranscriptAssistantKind::FinalAnswer)
    );
    let display = registry
        .conversation_display_page(&opened.id, None, 50)
        .expect("production canonical display should project");
    assert_eq!(display.request_scope, opened.id);
    assert_eq!(display.through_session_stream_sequence, "8");
    assert_eq!(display.total_items, "2");
    assert_eq!(display.items.len(), 2);
    assert_eq!(display.items[1].display_order.session_stream_sequence, "4");
    assert!(display.live_provisional_anchor.is_none());
    let task = display
        .task_control
        .as_ref()
        .expect("paused Task controls should restore from durable truth");
    assert_eq!(task.task_id, task_id.as_str());
    assert_eq!(task.status, "paused");
    assert_eq!(task.plan_version, Some(1));
    assert_eq!(task.steps[0].status.as_deref(), Some("interrupted"));
    assert!(task.can_continue);
    let serialized_display =
        serde_json::to_string(&display).expect("canonical display should serialize");
    assert!(!serialized_display.contains(&durable_session_id));
    assert!(!serialized_display.contains(private_objective));
    assert!(!serialized_display.contains("private planner detail"));
    assert!(!serialized_display.contains("private participant summary"));
    assert!(!serialized_display.contains("parent.jsonl"));
    assert_eq!(
        registry.conversation_display_page(&opened.id, Some("e30"), 50),
        Err(crate::HttpRegistryError::ConversationDisplayCursorInvalid)
    );
    assert_eq!(
        std::path::Path::new(&opened.session_log_path),
        session_path
            .canonicalize()
            .expect("session path should resolve")
    );
    let recovery_binding = attachment_recovery.recovery_binding.clone();
    drop(external_attachment);
    let replacement_attachment =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_path,
        )
        .expect("replacement controller should advance the attachment generation");
    let refreshed_recovery_binding = match registry.open_session(HttpSessionOpenRequest {
        recovery_binding: Some(recovery_binding),
        ..request.clone()
    }) {
        Err(crate::HttpRegistryError::SessionRunRecoveryRequired { recovery }) => {
            assert_eq!(
                recovery.code,
                crate::HttpSessionRouteRecoveryCode::SessionAlreadyActive
            );
            recovery.recovery_binding
        }
        other => panic!("stale retry must return a refreshed exact binding: {other:?}"),
    };
    drop(replacement_attachment);
    let activated = registry
        .open_session(HttpSessionOpenRequest {
            recovery_binding: Some(refreshed_recovery_binding),
            ..request.clone()
        })
        .expect("exact retry should activate the existing read handle");
    assert_eq!(activated.id, opened.id);
    assert!(activated.route_recovery.is_none());
    assert_eq!(
        activated
            .route_transition
            .as_ref()
            .map(|transition| transition.kind),
        Some(crate::HttpSessionRouteTransitionKind::Exact)
    );
    assert_eq!(
        registry
            .open_session(request)
            .expect("duplicate reopen should be idempotent")
            .id,
        opened.id
    );
    assert_eq!(
        registry.open_session(HttpSessionOpenRequest {
            session_ref: "session-history.jsonl".to_owned(),
            session_id: "stale-id".to_owned(),
            label: None,
            recovery_binding: None,
        }),
        Err(crate::HttpRegistryError::DurableSessionIdentityChanged)
    );
}

#[tokio::test]
async fn production_open_reports_one_automatic_same_trust_route_rebind() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let session_path = sessions.join("session-rebind.jsonl");
    let (binding, attachment) =
        sigil_runtime::application_run::bind_application_session_with_model_ref_and_attachment(
            &config_path,
            temp.path(),
            Some(&session_path),
            None,
            None,
        )
        .expect("application binding should persist the initial route trust boundary");
    let durable_session_id = binding.session_scope_id;
    drop(attachment);

    let changed = std::fs::read_to_string(&config_path)
        .expect("original config should read")
        .replace(
            "base_url = \"http://127.0.0.1:1\"",
            "base_url = \"http://127.0.0.1:1/v2\"",
        );
    std::fs::write(&config_path, changed).expect("same-trust route config should update");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-rebind.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures-rebind.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        "workspace-rebind",
        &sessions,
        temp.path().join("exports-rebind"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path())
                .with_session_lifecycle(lifecycle),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-rebind.json"), 8)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let request = HttpSessionOpenRequest {
        session_ref: "session-rebind.jsonl".to_owned(),
        session_id: durable_session_id,
        label: Some("Rebound".to_owned()),
        recovery_binding: None,
    };

    let opened = registry
        .open_session(request.clone())
        .expect("same-trust drift should rebind automatically");
    let receipt = opened
        .route_transition
        .as_ref()
        .expect("automatic rebind should return a transition receipt");
    assert_eq!(receipt.kind, crate::HttpSessionRouteTransitionKind::Rebound);
    assert_eq!(receipt.connection_id.as_deref(), Some("local-test"));
    assert_eq!(receipt.model_id.as_deref(), Some("gpt-test"));
    assert!(receipt.remote_context_reset);
    assert!(opened.route_recovery.is_none());
    let rebound_count = sigil_kernel::JsonlSessionStore::read_entries(&session_path)
        .expect("rebound session should remain readable")
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(ControlEntry::SessionRouteRebound { .. })
            )
        })
        .count();
    assert_eq!(rebound_count, 1);

    let reopened = registry
        .open_session(request)
        .expect("repeat open should be idempotent");
    assert_eq!(
        reopened
            .route_transition
            .as_ref()
            .map(|transition| transition.kind),
        Some(crate::HttpSessionRouteTransitionKind::Exact)
    );
    assert_eq!(
        sigil_kernel::JsonlSessionStore::read_entries(&session_path)
            .expect("idempotently reopened session should remain readable")
            .iter()
            .filter(|entry| matches!(
                entry,
                sigil_kernel::SessionLogEntry::Control(ControlEntry::SessionRouteRebound { .. })
            ))
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_driver_projects_and_executes_real_verification_rerun() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace should create");
    std::fs::write(workspace.join("note.txt"), "current\n").expect("fixture should write");
    let workspace = workspace.canonicalize().expect("workspace should resolve");
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, "workspace");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 8)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let adapter_session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let store = sigil_kernel::JsonlSessionStore::new(&adapter_session.session_log_path)
        .expect("session store should open");
    let (provider_name, route) = production_test_model_route(&temp);
    let mut session =
        sigil_kernel::Session::load_from_store(provider_name, route.model_ref.model_id, store)
            .expect("session should load");
    let task_id = TaskId::new("task_1").expect("task id");
    let step_id = TaskStepId::new("verify_1").expect("step id");
    let scope = EvidenceScope::Step("task_1:verify_1".to_owned());
    session
        .append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("session ref"),
            objective: "verify workspace".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        }))
        .expect("task run should append");
    session
        .append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "verify".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Verify),
                isolation: None,
            }],
            reason: None,
        }))
        .expect("task plan should append");
    session
        .append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Blocked,
            title: Some("verify".to_owned()),
            summary: None,
            reason: None,
        }))
        .expect("task step should append");
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )
    .expect("configured check should promote");
    let check_spec = trusted.check_spec.clone();
    session
        .append_control(ControlEntry::CheckSpecRecorded(
            CheckSpecRecordedEntry::new(
                EvidenceScope::Task(task_id.as_str().to_owned()),
                trusted,
                "event-config",
            ),
        ))
        .expect("check spec should append");
    let mut policy = VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = vec![check_spec.clone()];
    policy.completion_criteria = CompletionCriteria::AllRequiredChecks;
    policy.allow_unverified_completion = false;
    policy.timeout_ms = Some(60_000);
    let policy_entry = VerificationPolicyChangedEntry::new(
        EvidenceScope::Task(task_id.as_str().to_owned()),
        policy.clone(),
        "event-policy",
    )
    .expect("policy should hash");
    let policy_hash = policy_entry.policy_hash.clone();
    session
        .append_control(ControlEntry::VerificationPolicyChanged(policy_entry))
        .expect("policy should append");
    let workspace_id = stable_workspace_id(&workspace).expect("workspace id");
    let snapshot =
        build_workspace_snapshot(&workspace, workspace_id, &policy.verification_scope, 0)
            .expect("workspace should snapshot");
    let snapshot_id = snapshot
        .workspace_snapshot_id
        .expect("snapshot should have identity");
    session
        .append_control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
            scope,
            evaluation: ReadinessEvaluation {
                run_status: RunStatus::Completed,
                verification_verdict: VerificationVerdict::Missing,
                visible_state: VisibleCompletionState::CompletedUnverified,
                reasons: Vec::new(),
                required_actions: vec![RequiredAction::RunCheck {
                    check_spec_id: check_spec.check_spec_id.clone(),
                }],
            },
            policy_hash: Some(policy_hash),
            workspace_snapshot_id: Some(snapshot_id),
        }))
        .expect("readiness should append");
    drop(session);

    let rendered = registry
        .verification_view(&adapter_session.id)
        .expect("verification should project")
        .expect("verification should exist");
    let VerificationProductAction::Rerun(request) = rendered.action.expect("rerun action") else {
        panic!("expected exact rerun action");
    };
    let command = crate::HttpCommandEnvelope::new(
        "verification-real-1",
        "desktop-test",
        &adapter_session.id,
        request,
    );
    let registry_for_rerun = Arc::clone(&registry);
    let session_id = adapter_session.id.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        registry_for_rerun.rerun_verification_command(&session_id, command)
    })
    .await
    .expect("rerun worker should join")
    .expect("real verification should execute");

    assert_eq!(receipt.verification.status, "passed");
    assert!(receipt.verification.action.is_none());
    assert_eq!(
        receipt.verification.evidence.check_status,
        Some(sigil_kernel::VerificationCheckRunStatus::Succeeded)
    );
    assert!(receipt.verification.evidence.receipt_id.is_some());
    assert!(
        receipt
            .verification
            .evidence
            .workspace_snapshot_id
            .is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparation_deadline_quarantines_before_ack_and_retains_the_owner_for_reaping() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 16)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut options = HttpProductionRunDriverOptions::new(&config_path, temp.path());
    options.cancellation_timeout = Duration::from_millis(40);
    let driver = Arc::new(
        HttpProductionRunDriver::new_with_preparer(
            options,
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
            Arc::new(ControlledPreparation {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 16)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should bind");
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "wait in preparation".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect("run should start");
    started
        .acquire()
        .await
        .expect("preparation should start")
        .forget();

    let cancel_registry = Arc::clone(&registry);
    let run_id = run.id.clone();
    let cancel = tokio::task::spawn_blocking(move || cancel_registry.cancel_run(&run_id));
    let result = tokio::time::timeout(Duration::from_millis(400), cancel)
        .await
        .expect("cancel caller must return at the configured deadline")
        .expect("cancel worker should join");
    assert!(matches!(
        result,
        Err(crate::HttpRegistryError::DriverRejected {
            operation: "cancel",
            ..
        })
    ));
    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::ExecutionUncertain
    );
    assert_eq!(
        driver
            .active_run_count()
            .expect("active owners should remain observable"),
        1,
        "the timed-out preparation owner must remain held until it is reaped"
    );

    release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if driver.active_run_count().expect("active runs should read") == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released preparation should be reaped");
    assert_eq!(
        registry.get_run(&run.id).expect("run should exist").status,
        HttpRunStatus::ExecutionUncertain
    );
}

#[test]
fn approval_protocol_event_exposes_the_exact_guard_required_by_the_endpoint() {
    let bus = HttpLiveEventBus::new(8);
    let call = call();
    let spec = spec(ToolAccess::Write, None);
    let identity = approval_identity();
    let pending = HttpPendingApproval {
        call_id: call.id.clone(),
        tool_name: spec.name.clone(),
        approval_request_id: identity.approval_request_id.clone(),
        tool_call_hash: "b".repeat(64),
        policy_version: identity.policy_version.clone(),
        expires_at_ms: identity.expires_at_ms,
        session_grant_available: false,
        session_grant_unavailable_reason: unavailable_session_grant_reason(),
        display: crate::HttpPendingApprovalDisplay {
            event_sequence: 1,
            ..Default::default()
        },
    };
    let event = PublicRunEvent::new(
        "durable-session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            approval_identity: identity,
            session_grant_available: false,
            session_grant_unavailable_reason: unavailable_session_grant_reason(),
            effects: Default::default(),
            analysis: sigil_kernel::ToolAnalysisStatus::Complete,
            containment: Default::default(),
            safe_summary: Default::default(),
            decision_reasons: Vec::new(),
            call,
            spec,
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    );

    let published = bus
        .publish_run_event_with_approval(event, Some(pending.clone()))
        .expect("matching HTTP approval guard should publish");

    assert_eq!(published.approval_request, Some(pending));
    assert!(matches!(
        published.view(),
        crate::HttpProtocolEventView::Durable(view) if view.approval_request.is_some()
    ));
}

#[test]
fn approval_protocol_event_rejects_guard_for_another_call() {
    let bus = HttpLiveEventBus::new(8);
    let call = call();
    let spec = spec(ToolAccess::Write, None);
    let identity = approval_identity();
    let event = PublicRunEvent::new(
        "durable-session-1",
        "run-1",
        1,
        PublicRunEventKind::ApprovalRequested {
            approval_identity: identity.clone(),
            session_grant_available: false,
            session_grant_unavailable_reason: unavailable_session_grant_reason(),
            effects: Default::default(),
            analysis: sigil_kernel::ToolAnalysisStatus::Complete,
            containment: Default::default(),
            safe_summary: Default::default(),
            decision_reasons: Vec::new(),
            call,
            spec,
            subjects: Vec::new(),
            network_effect: None,
            local_policy_decision: None,
            network_policy_decision: None,
            source_policy_decision: None,
            operation: None,
            risk: None,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            preview: None,
        },
    );
    let wrong = HttpPendingApproval {
        call_id: "call-other".to_owned(),
        tool_name: "read_file".to_owned(),
        approval_request_id: identity.approval_request_id,
        tool_call_hash: "b".repeat(64),
        policy_version: identity.policy_version,
        expires_at_ms: identity.expires_at_ms,
        session_grant_available: false,
        session_grant_unavailable_reason: unavailable_session_grant_reason(),
        display: crate::HttpPendingApprovalDisplay {
            event_sequence: 1,
            ..Default::default()
        },
    };

    assert!(matches!(
        bus.publish_run_event_with_approval(event, Some(wrong)),
        Err(crate::HttpEventPublishError::ApprovalMetadata)
    ));
}

#[tokio::test]
async fn production_driver_projects_pre_started_runtime_preparation_failure_without_forging_domain_terminal()
 {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_preparation_failure_config(&config_path, ".");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 16)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let mut subscriber = event_bus.subscribe();
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "hello".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect("owned production supervisor should accept the run");

    // Provider preparation and typed recovery publication each cross a blocking-runtime handoff;
    // under the full workspace test fan-out both can be delayed without changing the outcome.
    let preparation_event = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let event = subscriber
                .recv()
                .await
                .expect("production failure event should remain observable");
            if event
                .run_event
                .as_ref()
                .expect("public event payload")
                .run_id
                == run.id
                && matches!(
                    &event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .event,
                    PublicRunEventKind::RouteRecoveryRequired {
                        code: PublicRouteRecoveryCode::ProviderUnavailable,
                        ..
                    }
                )
            {
                break event;
            }
        }
    })
    .await
    .expect("pre-Started preparation failure should publish its typed recovery promptly");
    assert!(matches!(
        preparation_event
            .run_event
            .as_ref()
            .expect("public event payload")
            .event,
        PublicRunEventKind::RouteRecoveryRequired {
            code: PublicRouteRecoveryCode::ProviderUnavailable,
            ..
        }
    ));

    let projected_status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = registry.get_run(&run.id).expect("run should exist").status;
            if status.is_terminal() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("typed preparation event should be followed by the terminal run projection");
    assert_eq!(projected_status, HttpRunStatus::Failed);
    assert!(session.session_log_path.ends_with(".jsonl"));
    let replay = event_bus
        .replay_run_after(&session.durable_session_scope_id, &run.id, None)
        .expect("typed preparation failure should remain replayable by HTTP");
    assert!(matches!(
        replay.last().map(|event| &event
            .run_event
            .as_ref()
            .expect("public event payload")
            .event),
        Some(PublicRunEventKind::RouteRecoveryRequired {
            code: PublicRouteRecoveryCode::ProviderUnavailable,
            ..
        })
    ));
    assert!(
        JsonlSessionStore::read_event_records(&session.session_log_path)
            .expect("preparation failure session records should read")
            .iter()
            .all(|record| record.stored_event().event_kind()
                != Some(sigil_kernel::DurableEventType::RunFinalized)),
        "pre-Started preparation failure must not forge a durable domain terminal"
    );
}

#[tokio::test]
async fn production_driver_bounded_network_failure_uses_durable_paused_terminal_and_exact_http_delivery()
 {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_bounded_network_failure_config(&config_path, ".");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 16)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let mut subscriber = event_bus.subscribe();
    let run = registry
        .start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "hello".to_owned(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect("owned production supervisor should accept the run");

    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = subscriber
                .recv()
                .await
                .expect("network recovery terminal should remain observable");
            if event
                .run_event
                .as_ref()
                .expect("public event payload")
                .run_id
                == run.id
                && matches!(
                    &event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .event,
                    PublicRunEventKind::RunPaused { .. }
                )
            {
                break event;
            }
        }
    })
    .await
    .expect("bounded network recovery should terminalize promptly");
    assert!(matches!(
        terminal
            .run_event
            .as_ref()
            .expect("public event payload")
            .event,
        PublicRunEventKind::RunPaused { .. }
    ));

    let projected_status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = registry.get_run(&run.id).expect("run should exist").status;
            if status.is_terminal() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("durable terminal should be followed by the registry projection");
    assert_eq!(projected_status, HttpRunStatus::Paused);

    let records = JsonlSessionStore::read_event_records(&session.session_log_path)
        .expect("network terminal records should read");
    let projection = PublicEventOutboxProjectionV1::from_records(&records)
        .expect("network terminal must retain an exact domain/outbox pair");
    assert!(projection.pending_for_adapter("http").is_empty());
    assert!(projection.events_in_order().iter().any(|entry| {
        entry.run_id == run.id && matches!(&entry.event.event, PublicRunEventKind::RunPaused { .. })
    }));
    let replay = event_bus
        .replay_run_after(&session.durable_session_scope_id, &run.id, None)
        .expect("durable HTTP terminal should remain replayable");
    assert!(matches!(
        replay.last().map(|event| &event
            .run_event
            .as_ref()
            .expect("public event payload")
            .event),
        Some(PublicRunEventKind::RunPaused { .. })
    ));
}

#[test]
fn production_execution_without_a_durable_terminal_is_refused_without_forging_failure() {
    let error = require_durable_application_terminal(None, "application execution ended")
        .expect_err("missing domain terminal must not be rewritten as RunFailed");

    assert!(
        error
            .to_string()
            .contains("without an atomically committed domain terminal and public outbox")
    );
}

#[test]
fn durable_application_terminal_maps_every_status_without_losing_failed_or_input_required() {
    let mappings = [
        (
            ApplicationRunTerminalStatus::Succeeded,
            HttpRunTerminalOutcome::Finished,
        ),
        (
            ApplicationRunTerminalStatus::Failed,
            HttpRunTerminalOutcome::Failed,
        ),
        (
            ApplicationRunTerminalStatus::Cancelled,
            HttpRunTerminalOutcome::Cancelled,
        ),
        (
            ApplicationRunTerminalStatus::Interrupted,
            HttpRunTerminalOutcome::Interrupted,
        ),
        (
            ApplicationRunTerminalStatus::Paused,
            HttpRunTerminalOutcome::Paused,
        ),
        (
            ApplicationRunTerminalStatus::Blocked,
            HttpRunTerminalOutcome::Blocked,
        ),
        (
            ApplicationRunTerminalStatus::AwaitingUserInput,
            HttpRunTerminalOutcome::Paused,
        ),
    ];

    for (status, expected) in mappings {
        assert_eq!(
            http_terminal_from_durable_application_status(status),
            expected
        );
    }
}

#[test]
fn plan_review_revision_interrupted_terminal_is_not_collapsed_to_failed() {
    let outcome = sigil_runtime::PlanReviewRunOutcome::Interrupted(
        "review worker reached its turn limit".to_owned(),
    );

    assert!(matches!(
        sigil_runtime::PlanReviewCoordinator::revision_terminal_public_event(&outcome),
        Some(PublicRunEventKind::RunInterrupted { reason })
            if reason == "review worker reached its turn limit"
    ));
}

#[test]
fn plan_review_revision_handler_binds_only_its_actual_live_source() -> Result<()> {
    use sigil_kernel::EventHandler;
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("preview.jsonl"))?;
    let session = sigil_kernel::Session::load_from_store("fixture", "model", store)?;
    let mut recorder =
        sigil_runtime::ApplicationRunEventRecorder::start(&session, "revision-preview", "revise")?;
    recorder.begin_live_attempt("revision-physical-attempt")?;
    recorder.handle(sigil_kernel::RunEvent::TextDelta("revision preview".into()))?;
    let event_bus = Arc::new(HttpLiveEventBus::new(8));
    let mut handler = HttpPlanReviewRevisionEventHandler {
        durable_session_scope_id: session.session_scope_id().to_owned(),
        run_id: "revision-preview".to_owned(),
        event_bus: Arc::clone(&event_bus),
    };
    handler.bind_live_preview_source(recorder.live_preview_source())?;
    let mut reader = event_bus
        .live_preview_reader(session.session_scope_id(), "revision-preview")?
        .expect("bound source");
    let updates = reader.poll_updates()?;
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].preview.as_str(), "revision preview");
    assert_eq!(updates[0].attempt_id, "revision-physical-attempt");
    handler.run_id = "another-revision".to_owned();
    assert!(
        handler
            .bind_live_preview_source(recorder.live_preview_source())
            .is_err()
    );
    assert!(
        event_bus
            .live_preview_reader(session.session_scope_id(), "another-revision")?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn plan_review_waiting_input_closes_only_live_delivery_and_preserves_exact_resume_sequence()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let journal = Arc::new(HttpDurableProtocolJournal::open(
        temp.path().join("plan-review-waiting-protocol.json"),
        16,
    )?);
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        8,
        Arc::clone(&journal),
    ));
    let mut subscriber = event_bus.subscribe();
    let mut handler = HttpPlanReviewRevisionEventHandler {
        durable_session_scope_id: "session-plan-review-waiting".to_owned(),
        run_id: "plan-review-revision-waiting".to_owned(),
        event_bus: Arc::clone(&event_bus),
    };

    handler.handle_public_event(PublicRunEvent::new(
        "session-plan-review-waiting",
        "plan-review-revision-waiting",
        7,
        PublicRunEventKind::RunAwaitingUserInput {
            request_id: "research-input-1".to_owned(),
            generation: 1,
            request_hash: "sha256:waiting-input".to_owned(),
        },
    ))?;

    let waiting = match subscriber.recv_run_stream().await? {
        crate::sse::HttpRunStreamReceive::Event(event) => event,
        crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {
            panic!("waiting event must precede its live stream close")
        }
    };
    assert_eq!(
        waiting
            .run_event
            .as_ref()
            .expect("public event payload")
            .sequence,
        7
    );
    assert!(matches!(
        waiting
            .run_event
            .as_ref()
            .expect("public event payload")
            .event,
        PublicRunEventKind::RunAwaitingUserInput { .. }
    ));
    assert!(matches!(
        subscriber.recv_run_stream().await?,
        crate::sse::HttpRunStreamReceive::StreamClosed { session_id, run_id }
            if session_id == "session-plan-review-waiting"
                && run_id == "plan-review-revision-waiting"
    ));
    assert_eq!(
        event_bus.latest_run_sequence(
            "session-plan-review-waiting",
            "plan-review-revision-waiting"
        )?,
        Some(7),
        "live close must not erase the durable resume watermark"
    );

    // A fresh handler represents the resumed supervisor after the exact user-input command. It
    // keeps the original logical run and appends rather than reopening a fake sequence one.
    let mut resumed = HttpPlanReviewRevisionEventHandler {
        durable_session_scope_id: "session-plan-review-waiting".to_owned(),
        run_id: "plan-review-revision-waiting".to_owned(),
        event_bus: Arc::clone(&event_bus),
    };
    resumed.handle_public_event(PublicRunEvent::new(
        "session-plan-review-waiting",
        "plan-review-revision-waiting",
        8,
        PublicRunEventKind::RunFinished {
            final_text: "revision resumed with the submitted research answer".to_owned(),
        },
    ))?;
    let replay = event_bus.replay_run_after(
        "session-plan-review-waiting",
        "plan-review-revision-waiting",
        None,
    )?;
    assert_eq!(
        replay
            .iter()
            .map(|event| event
                .run_event
                .as_ref()
                .expect("public event payload")
                .sequence)
            .collect::<Vec<_>>(),
        vec![7, 8]
    );
    Ok(())
}

#[test]
fn terminal_outbox_replay_keeps_the_original_gapped_http_sequence() -> anyhow::Result<()> {
    let event_bus = HttpLiveEventBus::new(8);
    event_bus.publish_run_event(PublicRunEvent::new(
        "session-terminal-replay",
        "run-terminal-replay",
        3,
        PublicRunEventKind::RunStarted {
            prompt: "start".to_owned(),
        },
    ))?;
    let terminal = PublicRunEvent::new(
        "session-terminal-replay",
        "run-terminal-replay",
        9,
        PublicRunEventKind::RunInterrupted {
            reason: "delivery recovered".to_owned(),
        },
    );

    publish_exact_http_outbox_event(
        &event_bus,
        "session-terminal-replay",
        "run-terminal-replay",
        terminal.clone(),
    )?;
    publish_exact_http_outbox_event(
        &event_bus,
        "session-terminal-replay",
        "run-terminal-replay",
        terminal,
    )?;

    let replay =
        event_bus.replay_run_after("session-terminal-replay", "run-terminal-replay", None)?;
    assert_eq!(
        replay
            .iter()
            .map(|event| event
                .run_event
                .as_ref()
                .expect("public event payload")
                .sequence)
            .collect::<Vec<_>>(),
        vec![3, 9]
    );
    Ok(())
}

#[tokio::test]
async fn full_public_outbox_replay_recovers_nonterminal_http_publication_before_receipt()
-> anyhow::Result<()> {
    fn entry(
        session_id: &str,
        run_id: &str,
        sequence: u64,
        message: &str,
    ) -> anyhow::Result<sigil_kernel::PublicEventOutboxEntryV1> {
        let event = PublicRunEvent::new(
            session_id,
            run_id,
            sequence,
            PublicRunEventKind::Notice {
                message: message.to_owned(),
            },
        );
        let public_event_id = format!("http-public:{session_id}:{run_id}:{sequence}");
        Ok(sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.clone(),
            domain_event_id: public_event_id,
            run_id: run_id.to_owned(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
            event,
        })
    }

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new("http-test", "test-model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    drop(session);
    let recorder = sigil_kernel::PublicEventOutboxRecorder::new(store.clone());
    let first = entry(&session_id, "run-first", 1, "first durable notice")?;
    let second = entry(&session_id, "run-second", 1, "second durable notice")?;
    recorder.append_outbox(&first)?;
    recorder.append_outbox(&second)?;

    let protocol_journal = Arc::new(HttpDurableProtocolJournal::open(
        temp.path().join("protocol.json"),
        8,
    )?);
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let mut subscriber = event_bus.subscribe();

    // Simulate process death after the canonical HTTP journal append but before the outbox
    // receipt. Equal bytes are accepted during recovery; the event must not be sent twice.
    publish_exact_http_outbox_event(event_bus.as_ref(), &session_id, "run-first", first.event)?;
    let first_live = match subscriber.recv_run_stream().await? {
        crate::sse::HttpRunStreamReceive::Event(event) => event,
        crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {
            panic!("nonterminal public event must reach the live HTTP consumer")
        }
    };
    assert_eq!(
        first_live
            .run_event
            .as_ref()
            .expect("public event payload")
            .run_id,
        "run-first"
    );

    assert_eq!(
        replay_pending_http_public_outboxes(&session_path, &session_id, &event_bus, None)?,
        2,
        "the first matching journal event and the second new event both receive ordered receipts"
    );
    let second_live = match subscriber.recv_run_stream().await? {
        crate::sse::HttpRunStreamReceive::Event(event) => event,
        crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {
            panic!("the later pending public event must not be overtaken or dropped")
        }
    };
    assert_eq!(
        second_live
            .run_event
            .as_ref()
            .expect("public event payload")
            .run_id,
        "run-second"
    );
    assert_eq!(
        second_live
            .run_event
            .as_ref()
            .expect("public event payload")
            .sequence,
        1
    );

    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_event_records_writer()?,
    )?;
    assert!(projection.pending_for_adapter("http").is_empty());
    assert_eq!(
        event_bus
            .replay_run_after(&session_id, "run-first", None)?
            .len(),
        1,
        "recovery must not duplicate the journal event already committed before its receipt"
    );
    assert_eq!(
        replay_pending_http_public_outboxes(&session_path, &session_id, &event_bus, None)?,
        0,
        "a restart after receipts are durable is idempotent"
    );
    Ok(())
}

#[tokio::test]
async fn evicted_http_projection_rebuilds_from_full_outbox_before_ordered_receipts()
-> anyhow::Result<()> {
    fn entry(
        session_id: &str,
        run_id: &str,
        sequence: u64,
        event: PublicRunEventKind,
    ) -> anyhow::Result<sigil_kernel::PublicEventOutboxEntryV1> {
        let event = PublicRunEvent::new(session_id, run_id, sequence, event);
        let public_event_id = format!("http-evicted:{session_id}:{run_id}:{sequence}");
        Ok(sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.clone(),
            domain_event_id: public_event_id,
            run_id: run_id.to_owned(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
            event,
        })
    }

    let temp = tempfile::tempdir()?;
    let run_id = "run-evicted-projection";
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new("http-test", "test-model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    drop(session);
    let recorder = sigil_kernel::PublicEventOutboxRecorder::new(store.clone());
    let source = vec![
        entry(
            &session_id,
            run_id,
            1,
            PublicRunEventKind::Notice {
                message: "durable one".to_owned(),
            },
        )?,
        entry(
            &session_id,
            run_id,
            2,
            PublicRunEventKind::Notice {
                message: "durable two".to_owned(),
            },
        )?,
        entry(
            &session_id,
            run_id,
            3,
            PublicRunEventKind::Notice {
                message: "durable three".to_owned(),
            },
        )?,
        entry(
            &session_id,
            run_id,
            4,
            PublicRunEventKind::Notice {
                message: "durable four".to_owned(),
            },
        )?,
        entry(
            &session_id,
            run_id,
            5,
            PublicRunEventKind::Notice {
                message: "durable five".to_owned(),
            },
        )?,
    ];
    for entry in &source {
        recorder.append_outbox(entry)?;
    }

    let journal = Arc::new(HttpDurableProtocolJournal::open(
        temp.path().join("protocol.json"),
        2,
    )?);
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, journal));
    // Simulate live publication before receipts followed by unrelated bounded-journal trimming.
    // This direct source delivery is intentionally not the recovery helper under test.
    for entry in &source {
        event_bus.publish_run_event(entry.event.clone())?;
    }
    assert!(matches!(
        event_bus.replay_run_after(&session_id, run_id, None),
        Err(crate::HttpProtocolReplayError::CursorExpired)
    ));

    let mut subscriber = event_bus.subscribe();
    assert_eq!(
        replay_pending_http_public_outboxes(&session_path, &session_id, &event_bus, None)?,
        source.len(),
        "all current durable source items must be re-attempted in order"
    );
    for expected_sequence in 1..=5 {
        let crate::sse::HttpRunStreamReceive::Event(event) = subscriber.recv_run_stream().await?
        else {
            panic!("rebuild must fan out every pending source item before its receipt");
        };
        assert_eq!(
            event
                .run_event
                .as_ref()
                .expect("public event payload")
                .sequence,
            expected_sequence
        );
        assert!(event.is_durable());
        assert!(event.replay_id.is_some());
    }
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_event_records_writer()?,
    )?;
    assert!(projection.pending_for_adapter("http").is_empty());
    assert!(
        matches!(
            event_bus.replay_run_after(&session_id, run_id, None),
            Err(crate::HttpProtocolReplayError::CursorExpired),
        ),
        "the rebuilt bounded durable window must retain normal expired-cursor semantics"
    );
    Ok(())
}

#[tokio::test]
async fn fully_evicted_closed_http_stream_rebuilds_pending_durable_outbox_events()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let run_id = "run-closed-and-evicted";
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new("http-test", "test-model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    drop(session);
    let source = (1..=2)
        .map(|sequence| {
            let event = PublicRunEvent::new(
                &session_id,
                run_id,
                sequence,
                PublicRunEventKind::Notice {
                    message: format!("durable notice {sequence}"),
                },
            );
            let public_event_id = format!("http-closed-evicted:{sequence}");
            Ok(sigil_kernel::PublicEventOutboxEntryV1 {
                schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                public_event_id: public_event_id.clone(),
                domain_event_id: public_event_id,
                run_id: run_id.to_owned(),
                sequence,
                payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
                event,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let recorder = sigil_kernel::PublicEventOutboxRecorder::new(store.clone());
    for entry in &source {
        recorder.append_outbox(entry)?;
    }

    let journal = Arc::new(HttpDurableProtocolJournal::open(
        temp.path().join("protocol.json"),
        1,
    )?);
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, journal));
    for entry in &source {
        event_bus.publish_run_event(entry.event.clone())?;
    }
    event_bus.close_run_stream(&session_id, run_id)?;
    event_bus.publish_run_event(PublicRunEvent::new(
        &session_id,
        "another-run",
        1,
        PublicRunEventKind::Notice {
            message: "displace the closed stream's final retained event".to_owned(),
        },
    ))?;
    event_bus.close_run_stream(&session_id, "another-run")?;
    assert!(
        event_bus
            .replay_run_after(&session_id, run_id, None)?
            .is_empty(),
        "a fully trimmed closed stream has no watermark, so replay(None) alone cannot prove delivery"
    );

    let mut subscriber = event_bus.subscribe();
    assert_eq!(
        replay_pending_http_public_outboxes(&session_path, &session_id, &event_bus, None)?,
        source.len(),
        "missing exact retained durable entries must rebuild even without CursorExpired"
    );
    for expected_sequence in 1..=2 {
        let crate::sse::HttpRunStreamReceive::Event(event) = subscriber.recv_run_stream().await?
        else {
            panic!("rebuilt durable outbox events must fan out before their receipts");
        };
        assert_eq!(
            event
                .run_event
                .as_ref()
                .expect("public event payload")
                .sequence,
            expected_sequence
        );
    }
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_event_records_writer()?,
    )?;
    assert!(projection.pending_for_adapter("http").is_empty());
    Ok(())
}

#[tokio::test]
async fn pending_terminal_lifecycle_replay_projects_registry_and_closes_before_receipt()
-> anyhow::Result<()> {
    struct RegistryOnlyDriver {
        binding: HttpSessionBinding,
    }

    impl HttpRunDriver for RegistryOnlyDriver {
        fn bind_session(
            &self,
            _session_id: &str,
            _model_ref: Option<&crate::HttpProviderModelRef>,
        ) -> Result<HttpSessionBinding, crate::HttpRunDriverError> {
            Ok(self.binding.clone())
        }

        fn start_run(
            &self,
            _start: crate::HttpRunDriverStart,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn cancel_run(
            &self,
            _cancel: crate::HttpRunDriverCancel,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn submit_approval(
            &self,
            _approval: crate::HttpRunDriverApproval,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }
    }

    fn outbox_entry(
        session_id: &str,
        run_id: &str,
        sequence: u64,
        event: PublicRunEventKind,
    ) -> anyhow::Result<sigil_kernel::PublicEventOutboxEntryV1> {
        let event = PublicRunEvent::new(session_id, run_id, sequence, event);
        let public_event_id = format!("http-lifecycle:{run_id}:{sequence}");
        Ok(sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.clone(),
            domain_event_id: public_event_id,
            run_id: run_id.to_owned(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
            event,
        })
    }

    fn lifecycle_entry(
        session_id: &str,
        run_id: &str,
        sequence: u64,
        task_id: &str,
    ) -> anyhow::Result<sigil_kernel::PublicEventOutboxEntryV1> {
        outbox_entry(
            session_id,
            run_id,
            sequence,
            PublicRunEventKind::TerminalLifecycle {
                event: sigil_kernel::TerminalLifecycleEvent {
                    task_id: sigil_kernel::TerminalTaskId::new(task_id)?,
                    execution_backend: None,
                    sandbox_profile: None,
                    generation: 1,
                    status: sigil_kernel::TerminalTaskStatus::Exited { exit_code: Some(0) },
                    readiness: sigil_kernel::TerminalReadinessStatus::None,
                    total_output_bytes: 0,
                    emitted_at_ms: sequence,
                },
            },
        )
    }

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new("http-test", "test-model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    drop(session);
    let run_id = "run-pending-terminal-lifecycle";
    let registry = Arc::new(HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: session_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    })));
    let adapter_session = registry.create_session(HttpSessionCreateRequest::default())?;
    let mutation = registry.reserve_durable_session_mutation(&session_id)?;
    registry.register_or_resume_supervised_revision_run(
        &adapter_session.id,
        run_id,
        HttpPermissionMode::ReadOnly,
        "pending lifecycle recovery",
        false,
    )?;
    drop(mutation);
    registry.record_run_terminal(run_id, HttpRunTerminalOutcome::Finished)?;

    let recorder = sigil_kernel::PublicEventOutboxRecorder::new(store.clone());
    let started = outbox_entry(
        &session_id,
        run_id,
        1,
        PublicRunEventKind::RunStarted {
            prompt: "pending lifecycle recovery".to_owned(),
        },
    )?;
    lifecycle.append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
        run_id, 1,
    )?)?;
    let foreground_terminal_entry = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        run_id,
        sigil_kernel::ConversationRunTerminalStatusV1::Succeeded,
        Some("pending-lifecycle-final-message".to_owned()),
        Some("foreground complete"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    let foreground_terminal_event = PublicRunEvent::new(
        &session_id,
        run_id,
        2,
        PublicRunEventKind::RunFinished {
            final_text: "foreground complete".to_owned(),
        },
    );
    let foreground_terminal = sigil_kernel::PublicEventOutboxEntryV1 {
        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!("http-lifecycle:{run_id}:2"),
        domain_event_id: format!("http-lifecycle-domain:{run_id}:2"),
        run_id: run_id.to_owned(),
        sequence: 2,
        payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(
            &foreground_terminal_event,
        )?),
        event: foreground_terminal_event,
    };
    recorder.append_outbox(&started)?;
    recorder.append_delivery(&sigil_kernel::PublicEventDeliveryReceiptV1 {
        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: started.public_event_id.clone(),
        adapter: "http".to_owned(),
        delivered_at_unix_ms: 1,
    })?;
    lifecycle.append_finalized_with_outbox(&foreground_terminal_entry, &foreground_terminal)?;
    recorder.append_delivery(&sigil_kernel::PublicEventDeliveryReceiptV1 {
        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: foreground_terminal.public_event_id.clone(),
        adapter: "http".to_owned(),
        delivered_at_unix_ms: 1,
    })?;
    let already_published = lifecycle_entry(&session_id, run_id, 3, "terminal-one")?;
    recorder.append_outbox(&already_published)?;
    let first_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        8,
        Arc::new(HttpDurableProtocolJournal::open(
            temp.path().join("protocol-existing.json"),
            8,
        )?),
    ));
    // Model a process death after journal append but before registry reduction, stream close, and
    // receipt. Replay must not publish a second lifecycle event, but it must finish the registry
    // transition and close this retained stream before writing the receipt.
    first_bus.publish_run_event(started.event.clone())?;
    first_bus.publish_run_event_with_stream_continuation(foreground_terminal.event.clone())?;
    first_bus.publish_run_event(already_published.event.clone())?;
    let mut first_subscriber = first_bus.subscribe();
    assert_eq!(
        replay_pending_http_public_outboxes(
            &session_path,
            &session_id,
            &first_bus,
            Some(&registry),
        )?,
        1
    );
    assert!(matches!(
        first_subscriber.recv_run_stream().await?,
        crate::sse::HttpRunStreamReceive::StreamClosed { run_id: ref closed, .. }
            if closed == run_id
    ));
    assert!(
        !first_bus.run_stream_accepts_events(&session_id, run_id)?,
        "an exact retained lifecycle retry must seal the previously-open stream"
    );
    assert_eq!(registry.get_run(run_id)?.terminal_tasks.len(), 1);

    let missing_from_journal = lifecycle_entry(&session_id, run_id, 4, "terminal-two")?;
    recorder.append_outbox(&missing_from_journal)?;
    let second_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        8,
        Arc::new(HttpDurableProtocolJournal::open(
            temp.path().join("protocol-missing.json"),
            8,
        )?),
    ));
    let mut second_subscriber = second_bus.subscribe();
    assert_eq!(
        replay_pending_http_public_outboxes(
            &session_path,
            &session_id,
            &second_bus,
            Some(&registry),
        )?,
        1
    );
    let crate::sse::HttpRunStreamReceive::Event(event) =
        second_subscriber.recv_run_stream().await?
    else {
        panic!("a missing final lifecycle must be delivered before stream close");
    };
    assert_eq!(
        event
            .run_event
            .as_ref()
            .expect("public event payload")
            .sequence,
        4
    );
    assert!(matches!(
        second_subscriber.recv_run_stream().await?,
        crate::sse::HttpRunStreamReceive::StreamClosed { run_id: ref closed, .. }
            if closed == run_id
    ));
    assert!(
        !second_bus.run_stream_accepts_events(&session_id, run_id)?,
        "a missing final lifecycle must append and close atomically before its receipt"
    );
    let projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_event_records_writer()?,
    )?;
    assert!(projection.pending_for_adapter("http").is_empty());
    assert_eq!(registry.get_run(run_id)?.terminal_tasks.len(), 2);
    let source = projection
        .events_in_order()
        .into_iter()
        .map(|entry| entry.event.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        registry.restore_terminal_lifecycle_projection_from_source(
            &source,
            &BTreeSet::from([(session_id.clone(), run_id.to_owned())]),
        )?,
        BTreeSet::from([(session_id.clone(), run_id.to_owned(), 4)]),
        "an already-applied source batch must still reconstruct the last lifecycle close rather than skipping every current generation"
    );
    let recovered_registry = Arc::new(HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: session_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    })));
    let recovered_session =
        recovered_registry.create_session(HttpSessionCreateRequest::default())?;
    let recovered_mutation = recovered_registry.reserve_durable_session_mutation(&session_id)?;
    recovered_registry.register_or_resume_supervised_revision_run(
        &recovered_session.id,
        run_id,
        HttpPermissionMode::ReadOnly,
        "pending lifecycle recovery",
        false,
    )?;
    drop(recovered_mutation);
    recovered_registry.record_run_terminal(run_id, HttpRunTerminalOutcome::Finished)?;
    let recovered_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        8,
        Arc::new(HttpDurableProtocolJournal::open(
            temp.path().join("protocol-registry-only.json"),
            8,
        )?),
    ));
    assert_eq!(
        replay_pending_http_public_outboxes(
            &session_path,
            &session_id,
            &recovered_bus,
            Some(&recovered_registry),
        )?,
        0,
        "receipt-complete lifecycle history should only rehydrate the registry"
    );
    assert_eq!(recovered_registry.get_run(run_id)?.terminal_tasks.len(), 2);
    assert!(
        recovered_bus
            .replay_run_after(&session_id, run_id, None)?
            .is_empty(),
        "registry-only lifecycle recovery must not republish historical lifecycle events"
    );
    let PublicRunEventKind::TerminalLifecycle {
        event: latest_lifecycle,
    } = &missing_from_journal.event.event
    else {
        unreachable!("test fixture must retain a terminal lifecycle")
    };
    let newer_running = sigil_kernel::TerminalLifecycleEvent {
        generation: latest_lifecycle.generation + 1,
        status: sigil_kernel::TerminalTaskStatus::Running,
        ..latest_lifecycle.clone()
    };
    registry.record_terminal_lifecycle(run_id, &newer_running)?;
    assert!(matches!(
        registry.restore_terminal_lifecycle_projection_from_source(
            &source,
            &BTreeSet::from([(session_id.clone(), run_id.to_owned())]),
        ),
        Err(HttpRegistryError::DriverRejected {
            operation: "plan terminal lifecycle replay",
            ..
        })
    ));

    // Rebuild from one complete durable source where the foreground terminal follows the task
    // lifecycle. The close belongs to RunFinished sequence four, not the earlier Exited event.
    let ordered_run_id = "run-source-ordered-terminal";
    let ordered_registry = Arc::new(HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: session_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    })));
    let ordered_session = ordered_registry.create_session(HttpSessionCreateRequest::default())?;
    let ordered_mutation = ordered_registry.reserve_durable_session_mutation(&session_id)?;
    ordered_registry.register_or_resume_supervised_revision_run(
        &ordered_session.id,
        ordered_run_id,
        HttpPermissionMode::ReadOnly,
        "source ordered lifecycle recovery",
        false,
    )?;
    drop(ordered_mutation);
    lifecycle.append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
        ordered_run_id,
        3,
    )?)?;
    let ordered_started = outbox_entry(
        &session_id,
        ordered_run_id,
        1,
        PublicRunEventKind::RunStarted {
            prompt: "source ordered lifecycle recovery".to_owned(),
        },
    )?;
    let ordered_running = outbox_entry(
        &session_id,
        ordered_run_id,
        2,
        PublicRunEventKind::TerminalLifecycle {
            event: sigil_kernel::TerminalLifecycleEvent {
                task_id: sigil_kernel::TerminalTaskId::new("ordered-terminal")?,
                execution_backend: None,
                sandbox_profile: None,
                generation: 1,
                status: sigil_kernel::TerminalTaskStatus::Running,
                readiness: sigil_kernel::TerminalReadinessStatus::None,
                total_output_bytes: 0,
                emitted_at_ms: 2,
            },
        },
    )?;
    let ordered_exited = outbox_entry(
        &session_id,
        ordered_run_id,
        3,
        PublicRunEventKind::TerminalLifecycle {
            event: sigil_kernel::TerminalLifecycleEvent {
                task_id: sigil_kernel::TerminalTaskId::new("ordered-terminal")?,
                execution_backend: None,
                sandbox_profile: None,
                generation: 2,
                status: sigil_kernel::TerminalTaskStatus::Exited { exit_code: Some(0) },
                readiness: sigil_kernel::TerminalReadinessStatus::None,
                total_output_bytes: 0,
                emitted_at_ms: 3,
            },
        },
    )?;
    let ordered_terminal_entry = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        ordered_run_id,
        sigil_kernel::ConversationRunTerminalStatusV1::Succeeded,
        Some("ordered-lifecycle-final-message".to_owned()),
        Some("foreground completed after terminal exit"),
        4,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    let ordered_finished_event = PublicRunEvent::new(
        &session_id,
        ordered_run_id,
        4,
        PublicRunEventKind::RunFinished {
            final_text: "foreground completed after terminal exit".to_owned(),
        },
    );
    let ordered_finished = sigil_kernel::PublicEventOutboxEntryV1 {
        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!("http-lifecycle:{ordered_run_id}:4"),
        domain_event_id: format!("http-lifecycle-domain:{ordered_run_id}:4"),
        run_id: ordered_run_id.to_owned(),
        sequence: 4,
        payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(
            &ordered_finished_event,
        )?),
        event: ordered_finished_event,
    };
    for entry in [&ordered_started, &ordered_running, &ordered_exited] {
        recorder.append_outbox(entry)?;
        recorder.append_delivery(&sigil_kernel::PublicEventDeliveryReceiptV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: entry.public_event_id.clone(),
            adapter: "http".to_owned(),
            delivered_at_unix_ms: 2,
        })?;
    }
    lifecycle.append_finalized_with_outbox(&ordered_terminal_entry, &ordered_finished)?;
    let ordered_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        8,
        Arc::new(HttpDurableProtocolJournal::open(
            temp.path().join("protocol-source-ordered.json"),
            8,
        )?),
    ));
    let mut ordered_subscriber = ordered_bus.subscribe();
    assert_eq!(
        replay_pending_http_public_outbox(
            &session_path,
            &session_id,
            ordered_run_id,
            &ordered_bus,
            &ordered_registry,
        )?,
        1
    );
    let crate::sse::HttpRunStreamReceive::Event(ordered_terminal) =
        ordered_subscriber.recv_run_stream().await?
    else {
        panic!("the pending foreground terminal must follow the rebuilt lifecycle source");
    };
    assert_eq!(
        ordered_terminal
            .run_event
            .as_ref()
            .expect("public event payload")
            .sequence,
        4
    );
    assert!(matches!(
        ordered_subscriber.recv_run_stream().await?,
        crate::sse::HttpRunStreamReceive::StreamClosed { run_id: ref closed, .. }
            if closed == ordered_run_id
    ));
    assert!(
        !ordered_bus.run_stream_accepts_events(&session_id, ordered_run_id)?,
        "the rebuilt candidate must atomically seal at RunFinished rather than Exited"
    );
    Ok(())
}

#[tokio::test]
async fn http_rejects_transient_public_outboxes_without_rewriting_or_acknowledging_them()
-> anyhow::Result<()> {
    for already_acknowledged in [false, true] {
        let temp = tempfile::tempdir()?;
        let run_id = "run-unsupported-transient";
        let session_path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&session_path)?;
        let mut session =
            sigil_kernel::Session::new("http-test", "test-model").with_store(store.clone());
        session.ensure_identity_entry()?;
        let session_id = session.session_scope_id().to_owned();
        drop(session);
        let event = PublicRunEvent::new(
            &session_id,
            run_id,
            1,
            PublicRunEventKind::TextDelta {
                text: "unsupported durable preview".to_owned(),
            },
        );
        let outbox = sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: "http-unsupported-transient:1".to_owned(),
            domain_event_id: "http-unsupported-transient:1".to_owned(),
            run_id: run_id.to_owned(),
            sequence: 1,
            payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
            event: event.clone(),
        };
        let before = std::fs::read(&session_path)?;
        assert!(
            sigil_kernel::PublicEventOutboxRecorder::new(store.clone())
                .append_outbox(&outbox)
                .is_err()
        );
        assert_eq!(std::fs::read(&session_path)?, before);
        let next_sequence = store
            .read_handle()
            .read_event_records()?
            .last()
            .expect("identity record")
            .stream_sequence()
            + 1;
        let raw = sigil_kernel::StoredEvent::new(
            sigil_kernel::DurableEventType::PublicEventOutbox,
            sigil_kernel::EventClass::Critical,
            outbox.public_event_id.clone(),
            session_id.clone(),
            next_sequence,
            serde_json::to_value(&outbox)?,
        )?;
        let mut original = before;
        original.extend_from_slice(raw.to_json_line()?.as_bytes());
        if already_acknowledged {
            let receipt = sigil_kernel::PublicEventDeliveryReceiptV1 {
                schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                public_event_id: outbox.public_event_id,
                adapter: "http".to_owned(),
                delivered_at_unix_ms: 1,
            };
            let raw_receipt = sigil_kernel::StoredEvent::new(
                sigil_kernel::DurableEventType::PublicEventDeliveryReceipt,
                sigil_kernel::EventClass::Critical,
                "unsupported-transient-receipt".to_owned(),
                session_id.clone(),
                next_sequence + 1,
                serde_json::to_value(receipt)?,
            )?;
            original.extend_from_slice(raw_receipt.to_json_line()?.as_bytes());
        }
        drop(store);
        // Materialize old bytes directly: the current writer must never manufacture them.
        std::fs::write(&session_path, &original)?;
        let journal = Arc::new(HttpDurableProtocolJournal::open(
            temp.path().join("protocol.json"),
            8,
        )?);
        let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, journal));
        assert!(publish_exact_http_outbox_event(&event_bus, &session_id, run_id, event).is_err());
        assert!(
            replay_pending_http_public_outboxes(&session_path, &session_id, &event_bus, None)
                .is_err()
        );
        assert_eq!(
            std::fs::read(&session_path)?,
            original,
            "unsupported history must not be migrated or acknowledged"
        );
        assert!(
            event_bus
                .latest_run_sequence(&session_id, run_id)?
                .is_none()
        );
        assert!(
            event_bus
                .replay_run_after(&session_id, run_id, None)?
                .is_empty()
        );
    }
    Ok(())
}

#[test]
fn revision_receipt_failure_still_reconciles_the_registered_terminal_run() -> anyhow::Result<()> {
    struct RegistryOnlyDriver {
        binding: HttpSessionBinding,
    }

    impl HttpRunDriver for RegistryOnlyDriver {
        fn bind_session(
            &self,
            _session_id: &str,
            _model_ref: Option<&crate::HttpProviderModelRef>,
        ) -> Result<HttpSessionBinding, crate::HttpRunDriverError> {
            Ok(self.binding.clone())
        }

        fn start_run(
            &self,
            _start: crate::HttpRunDriverStart,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn cancel_run(
            &self,
            _cancel: crate::HttpRunDriverCancel,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn submit_approval(
            &self,
            _approval: crate::HttpRunDriverApproval,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("revision-receipt-failure.jsonl");
    write_production_test_config(&temp.path().join("sigil.toml"), ".");
    let (provider_name, route) = production_test_model_route(&temp);
    let mut session = sigil_kernel::Session::load_from_store_with_route(
        provider_name,
        route.model_ref.model_id.clone(),
        Some(route),
        JsonlSessionStore::new(&session_path)?,
    )?;
    let durable_session_scope_id = session.session_scope_id().to_owned();
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        &durable_session_scope_id,
        "revision-receipt-failure-message",
        "revision-receipt-failure-origin-run",
    )?;
    let review_id = sigil_kernel::PlanReviewId::new("revision-receipt-failure-review")?;
    let attempt_id = sigil_kernel::PlanReviewAttemptId::new("revision-receipt-failure-attempt")?;
    let source = sigil_kernel::PlanSourceRef {
        source_turn: Some(source_turn.clone()),
        plan_review_id: Some(review_id.clone()),
        ..sigil_kernel::PlanSourceRef::default()
    };
    let base = sigil_kernel::plain_text_plan_draft_entry_with_plan_id(
        sigil_kernel::PlanId::new("revision-receipt-failure-base")?,
        "base plan",
        source,
        1,
        None,
    )?
    .expect("nonempty durable base plan");
    let requested = sigil_kernel::PlanDecisionRecordedEntry {
        plan_id: base.plan_id.clone(),
        plan_hash: base.plan_hash.clone(),
        decision: sigil_kernel::PlanDecision::RevisionRequested,
        decided_by: sigil_kernel::PlanDecisionActor::User,
        decided_at_ms: 2,
        reason: None,
    };
    let started_attempt = sigil_kernel::PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: sigil_kernel::PlanId::new("revision-receipt-failure-candidate")?,
        source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
        source_turn,
        explicit_objective: Some("base plan".to_owned()),
        route_decision_id: None,
        child_session_ref: sigil_kernel::plan_review_child_session_ref(&review_id, &attempt_id),
        finalizer_session_ref: Some(sigil_kernel::plan_review_finalizer_session_ref(
            &review_id,
            &attempt_id,
            1,
        )),
        revision_request_id: Some(sigil_kernel::UserInputRequestId::new(
            "revision-receipt-failure-request",
        )?),
        attempt_ordinal: 1,
        base_plan_id: Some(base.plan_id.clone()),
        base_plan_hash: Some(base.plan_hash.clone()),
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: sigil_kernel::PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 2,
    };
    session.append_controls(vec![
        ControlEntry::PlanDraftCreated(base.clone()),
        ControlEntry::PlanDecisionRecorded(requested),
        ControlEntry::PlanReviewAttempt(started_attempt.clone()),
    ])?;
    let run_id = sigil_kernel::plan_review_revision_run_id(&started_attempt);
    let terminal_sequence = session.next_plan_review_public_sequence(&run_id)?;
    let terminal_attempt = sigil_kernel::PlanReviewAttemptEntry {
        status: sigil_kernel::PlanReviewAttemptStatus::Cancelled,
        terminal_reason: Some(sigil_kernel::PlanReviewTerminalReason::UserCancelled),
        recorded_at_ms: 3,
        ..started_attempt
    };
    let terminal_decision = sigil_kernel::PlanDecisionRecordedEntry {
        plan_id: base.plan_id.clone(),
        plan_hash: base.plan_hash.clone(),
        decision: sigil_kernel::PlanDecision::RevisionFailed,
        decided_by: sigil_kernel::PlanDecisionActor::System,
        decided_at_ms: 3,
        reason: Some("user cancelled the revision".to_owned()),
    };
    let durable_outbox = session.append_plan_review_revision_terminal(
        terminal_attempt,
        None,
        terminal_decision,
        PublicRunEvent::new(
            &durable_session_scope_id,
            &run_id,
            terminal_sequence,
            PublicRunEventKind::RunCancelled,
        ),
    )?;
    let registry = HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: durable_session_scope_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    }));
    let adapter_session = registry.create_session(HttpSessionCreateRequest::default())?;
    let mutation = registry.reserve_durable_session_mutation(&durable_session_scope_id)?;
    registry.register_or_resume_supervised_revision_run(
        &adapter_session.id,
        &run_id,
        HttpPermissionMode::ReadOnly,
        "revision receipt failure fixture",
        false,
    )?;
    drop(mutation);

    let protocol_journal_path = temp.path().join("revision-receipt-failure.protocol.json");
    let event_bus = HttpLiveEventBus::with_durable_journal(
        8,
        Arc::new(HttpDurableProtocolJournal::open(&protocol_journal_path, 8)?),
    );
    publish_exact_http_outbox_event(
        &event_bus,
        &durable_session_scope_id,
        &run_id,
        durable_outbox.event.clone(),
    )?;
    let unavailable_session_path = temp.path().join("revision-receipt-failure.unavailable");
    std::fs::rename(&session_path, &unavailable_session_path)?;
    std::fs::create_dir(&session_path)?;
    let delivery = deliver_and_reconcile_plan_review_revision_terminal(
        &registry,
        &event_bus,
        &session_path,
        &durable_session_scope_id,
        &durable_outbox,
        true,
    );
    std::fs::remove_dir(&session_path)?;
    std::fs::rename(&unavailable_session_path, &session_path)?;
    let error =
        delivery.expect_err("receipt write failure must remain visible to the command caller");
    assert!(
        error.to_string().contains("delivery receipt failed"),
        "the independent delivery failure must not be erased: {error}"
    );
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let pending_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let pending = pending_projection.pending_for_adapter("http");
    assert!(pending.iter().any(|entry| {
        entry.public_event_id == durable_outbox.public_event_id
            && entry.domain_event_id == durable_outbox.domain_event_id
            && entry.payload_digest == durable_outbox.payload_digest
            && serde_json::to_value(&entry.event).ok()
                == serde_json::to_value(&durable_outbox.event).ok()
    }));
    assert_eq!(
        registry.get_run(&run_id)?.status,
        HttpRunStatus::Cancelled,
        "the exact durable terminal must still replace the registered revision state"
    );
    drop(event_bus);
    let recovery_event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        8,
        Arc::new(HttpDurableProtocolJournal::open(&protocol_journal_path, 8)?),
    ));
    assert_eq!(
        replay_pending_http_public_outboxes(
            &session_path,
            &durable_session_scope_id,
            &recovery_event_bus,
            None,
        )?,
        1,
        "attachment recovery must replay the existing exact terminal once"
    );
    assert_eq!(
        reconcile_registered_http_terminal_outboxes(
            &session_path,
            &durable_session_scope_id,
            &registry,
            &recovery_event_bus,
        )?,
        1,
        "attachment recovery must re-project the registered revision without inventing a terminal"
    );
    let recovered_records = JsonlSessionStore::read_event_records(&session_path)?;
    let recovered_projection =
        sigil_kernel::PublicEventOutboxProjectionV1::from_records(&recovered_records)?;
    assert!(
        !recovered_projection
            .pending_for_adapter("http")
            .into_iter()
            .any(|entry| entry.public_event_id == durable_outbox.public_event_id),
        "the recovered exact delivery must append the one HTTP receipt"
    );
    assert_eq!(
        recovered_projection
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.public_event_id == durable_outbox.public_event_id)
            .count(),
        1,
        "recovery must not append a replacement terminal outbox"
    );
    assert_eq!(
        recovery_event_bus
            .replay_run_after(&durable_session_scope_id, &run_id, None)?
            .iter()
            .map(|event| event
                .run_event
                .as_ref()
                .expect("public event payload")
                .sequence)
            .collect::<Vec<_>>(),
        vec![durable_outbox.event.sequence],
        "recovery must preserve rather than renumber or duplicate the exact terminal event"
    );
    Ok(())
}

#[test]
fn attachment_reconcile_keeps_a_durable_revision_waiting_checkpoint_resumable() -> anyhow::Result<()>
{
    struct RegistryOnlyDriver {
        binding: HttpSessionBinding,
    }

    impl HttpRunDriver for RegistryOnlyDriver {
        fn bind_session(
            &self,
            _session_id: &str,
            _model_ref: Option<&crate::HttpProviderModelRef>,
        ) -> Result<HttpSessionBinding, crate::HttpRunDriverError> {
            Ok(self.binding.clone())
        }

        fn start_run(
            &self,
            _start: crate::HttpRunDriverStart,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn cancel_run(
            &self,
            _cancel: crate::HttpRunDriverCancel,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn submit_approval(
            &self,
            _approval: crate::HttpRunDriverApproval,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("revision-waiting-attachment.jsonl");
    let mut session = sigil_kernel::Session::load_from_store(
        "test",
        "model",
        JsonlSessionStore::new(&session_path)?,
    )?;
    let durable_session_scope_id = session.session_scope_id().to_owned();
    let review_id = sigil_kernel::PlanReviewId::new("waiting-attachment-review")?;
    let attempt_id = sigil_kernel::PlanReviewAttemptId::new("waiting-attachment-revision")?;
    let base = sigil_kernel::plain_text_plan_draft_entry_with_plan_id(
        sigil_kernel::PlanId::new("waiting-attachment-base")?,
        "Original plan",
        sigil_kernel::PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("nonempty base plan");
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        &durable_session_scope_id,
        "waiting-attachment-source",
        "waiting-attachment-origin",
    )?;
    let started_attempt = sigil_kernel::PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: sigil_kernel::PlanId::new("waiting-attachment-candidate")?,
        source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
        source_turn,
        explicit_objective: Some("Original objective".to_owned()),
        route_decision_id: None,
        child_session_ref: sigil_kernel::plan_review_child_session_ref(&review_id, &attempt_id),
        finalizer_session_ref: None,
        revision_request_id: Some(sigil_kernel::UserInputRequestId::new(
            "waiting-attachment-guidance",
        )?),
        attempt_ordinal: 1,
        base_plan_id: Some(base.plan_id.clone()),
        base_plan_hash: Some(base.plan_hash.clone()),
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: sigil_kernel::PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 2,
    };
    session.append_controls(vec![
        ControlEntry::PlanDraftCreated(base.clone()),
        ControlEntry::PlanDecisionRecorded(sigil_kernel::PlanDecisionRecordedEntry {
            plan_id: base.plan_id.clone(),
            plan_hash: base.plan_hash.clone(),
            decision: sigil_kernel::PlanDecision::RevisionRequested,
            decided_by: sigil_kernel::PlanDecisionActor::User,
            decided_at_ms: 2,
            reason: None,
        }),
        ControlEntry::PlanReviewAttempt(started_attempt.clone()),
    ])?;
    let run_id = sigil_kernel::plan_review_revision_run_id(&started_attempt);
    let request_hash = sigil_kernel::stable_event_hash(b"waiting attachment request");
    let waiting_attempt = sigil_kernel::PlanReviewAttemptEntry {
        status: sigil_kernel::PlanReviewAttemptStatus::WaitingForInput,
        pending_user_input: Some(Box::new(sigil_kernel::PublicUserInputRequestV1 {
            identity: sigil_kernel::UserInputIdentityV1 {
                session_scope_id: sigil_kernel::SessionScopeId::new("child-session")?,
                root_logical_run_id: sigil_kernel::LogicalRunId::new(&run_id)?,
                source_thread_id: sigil_kernel::AgentThreadId::new("main")?,
                request_id: sigil_kernel::UserInputRequestId::new("waiting-attachment-question")?,
                generation: 1,
                source_binding_hash: sigil_kernel::stable_event_hash(b"waiting child binding"),
            },
            request_hash: request_hash.clone(),
            source: sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                plan_review_id: review_id,
                attempt_id,
            },
            purpose: sigil_kernel::UserInputPurposeV1::Clarification,
            prompt: "Which scope?".to_owned(),
            questions: vec![sigil_kernel::UserInputQuestionV1 {
                id: "scope".to_owned(),
                header: "Scope".to_owned(),
                question: "Which scope?".to_owned(),
                description: None,
                required: true,
                field: sigil_kernel::UserInputFieldKindV1::Text {
                    multiline: false,
                    max_chars: 256,
                },
            }],
            allowed_actions: vec![sigil_kernel::UserInputActionV1::Submit],
            requested_at_unix_ms: 3,
            status: sigil_kernel::UserInputStatusV1::Requested,
            answer_receipt: None,
            resolution: None,
        })),
        terminal_reason: None,
        recorded_at_ms: 3,
        ..started_attempt
    };
    session.append_plan_review_revision_waiting(
        waiting_attempt,
        PublicRunEvent::new(
            &durable_session_scope_id,
            &run_id,
            1,
            PublicRunEventKind::RunAwaitingUserInput {
                request_id: "waiting-attachment-question".to_owned(),
                generation: 1,
                request_hash,
            },
        ),
    )?;

    let registry = HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: durable_session_scope_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    }));
    let adapter_session = registry.create_session(HttpSessionCreateRequest::default())?;
    let mutation = registry.reserve_durable_session_mutation(&durable_session_scope_id)?;
    registry.register_or_resume_supervised_revision_run(
        &adapter_session.id,
        &run_id,
        HttpPermissionMode::ReadOnly,
        "revision waiting attachment fixture",
        false,
    )?;
    drop(mutation);
    registry.record_supervised_revision_waiting(&run_id)?;
    assert_eq!(registry.get_run(&run_id)?.status, HttpRunStatus::Paused);

    // This is the terminal-reconciliation operation invoked by session attachment. A Waiting
    // pair must neither close the run stream nor consume the registry's resumable checkpoint.
    assert_eq!(
        reconcile_registered_http_terminal_outboxes(
            &session_path,
            &durable_session_scope_id,
            &registry,
            &HttpLiveEventBus::new(8),
        )?,
        0
    );
    assert_eq!(registry.get_run(&run_id)?.status, HttpRunStatus::Paused);

    let mutation = registry.reserve_durable_session_mutation(&durable_session_scope_id)?;
    let resumed = registry.register_or_resume_supervised_revision_run(
        &adapter_session.id,
        &run_id,
        HttpPermissionMode::ReadOnly,
        "revision waiting attachment fixture",
        true,
    )?;
    drop(mutation);
    assert_eq!(resumed.status, HttpRunStatus::Running);
    Ok(())
}

#[test]
fn attachment_reprojects_registered_uncertain_run_after_terminal_receipt_was_already_acknowledged()
-> anyhow::Result<()> {
    struct RegistryOnlyDriver {
        binding: HttpSessionBinding,
    }

    impl HttpRunDriver for RegistryOnlyDriver {
        fn bind_session(
            &self,
            _session_id: &str,
            _model_ref: Option<&crate::HttpProviderModelRef>,
        ) -> Result<HttpSessionBinding, crate::HttpRunDriverError> {
            Ok(self.binding.clone())
        }

        fn start_run(
            &self,
            _start: crate::HttpRunDriverStart,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn cancel_run(
            &self,
            _cancel: crate::HttpRunDriverCancel,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn submit_approval(
            &self,
            _approval: crate::HttpRunDriverApproval,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }
    }

    fn append_acknowledged_terminal(
        lifecycle: &sigil_kernel::ConversationRunLifecycleRecorder,
        session_scope_id: &str,
        run_id: &str,
        terminal: &sigil_kernel::ConversationRunFinalizedEntryV1,
        event: PublicRunEvent,
        store: JsonlSessionStore,
    ) -> anyhow::Result<()> {
        let outbox = sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: format!("application-public:{session_scope_id}:{run_id}:1"),
            domain_event_id: format!("application-domain:{run_id}:1"),
            run_id: run_id.to_owned(),
            sequence: 1,
            payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
            event,
        };
        lifecycle.append_finalized_with_outbox(terminal, &outbox)?;
        sigil_kernel::PublicEventOutboxRecorder::new(store).append_delivery(
            &sigil_kernel::PublicEventDeliveryReceiptV1 {
                schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
                public_event_id: outbox.public_event_id,
                adapter: "http".to_owned(),
                delivered_at_unix_ms: 1,
            },
        )?;
        Ok(())
    }

    let temp = tempfile::tempdir()?;
    write_production_test_config(&temp.path().join("sigil.toml"), ".");
    let session_path = temp.path().join("attachment-terminal-replay.jsonl");
    let (provider_name, route) = production_test_model_route(&temp);
    let session_store = JsonlSessionStore::new(&session_path)?;
    let session =
        sigil_kernel::Session::new_with_route(provider_name, route).with_store(session_store);
    let durable_session_scope_id = session.session_scope_id().to_owned();
    let registry = HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: durable_session_scope_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    }));
    let adapter_session = registry.create_session(HttpSessionCreateRequest::default())?;
    let run = registry.start_run(
        &adapter_session.id,
        HttpRunStartRequest {
            prompt: "repair the terminal registry".to_owned(),
            permission_mode: Some(HttpPermissionMode::Manual),
            model_ref: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
            task_continuation: None,
        },
    )?;
    registry.record_run_execution_uncertain(&run.id)?;

    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
        &run.id, 1,
    )?)?;
    let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        &run.id,
        sigil_kernel::ConversationRunTerminalStatusV1::AwaitingUserInput,
        None,
        Some("exact input is required"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    append_acknowledged_terminal(
        &lifecycle,
        &durable_session_scope_id,
        &run.id,
        &terminal,
        PublicRunEvent::new(
            &durable_session_scope_id,
            &run.id,
            1,
            PublicRunEventKind::RunAwaitingUserInput {
                request_id: "input-request-1".to_owned(),
                generation: 1,
                request_hash: "input-request-hash-1".to_owned(),
            },
        ),
        JsonlSessionStore::new(&session_path)?,
    )?;

    // A durable historical terminal that has no process-local HTTP run must not be recreated.
    lifecycle.append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
        "durable-only-terminal",
        3,
    )?)?;
    let durable_only_terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        "durable-only-terminal",
        sigil_kernel::ConversationRunTerminalStatusV1::Failed,
        None,
        Some("historical failure"),
        4,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    append_acknowledged_terminal(
        &lifecycle,
        &durable_session_scope_id,
        "durable-only-terminal",
        &durable_only_terminal,
        PublicRunEvent::new(
            &durable_session_scope_id,
            "durable-only-terminal",
            1,
            PublicRunEventKind::RunFailed {
                error: "historical failure".to_owned(),
            },
        ),
        JsonlSessionStore::new(&session_path)?,
    )?;

    let projection = PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&session_path)?,
    )?;
    assert!(projection.pending_for_adapter("http").is_empty());

    let event_bus = HttpLiveEventBus::new(8);
    assert_eq!(
        reconcile_registered_http_terminal_outboxes(
            &session_path,
            &durable_session_scope_id,
            &registry,
            &event_bus,
        )?,
        1
    );
    assert_eq!(registry.get_run(&run.id)?.status, HttpRunStatus::Paused);
    assert!(matches!(
        registry.get_run("durable-only-terminal"),
        Err(crate::HttpRegistryError::RunNotFound { .. })
    ));
    Ok(())
}

#[test]
fn attachment_rejects_terminal_for_registered_run_bound_to_another_durable_session()
-> anyhow::Result<()> {
    struct MultipleBindingsDriver {
        bindings: Mutex<VecDeque<HttpSessionBinding>>,
    }

    impl HttpRunDriver for MultipleBindingsDriver {
        fn bind_session(
            &self,
            _session_id: &str,
            _model_ref: Option<&crate::HttpProviderModelRef>,
        ) -> Result<HttpSessionBinding, crate::HttpRunDriverError> {
            self.bindings
                .lock()
                .expect("test binding queue lock")
                .pop_front()
                .ok_or_else(|| crate::HttpRunDriverError::new("test binding queue exhausted"))
        }

        fn start_run(
            &self,
            _start: crate::HttpRunDriverStart,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn cancel_run(
            &self,
            _cancel: crate::HttpRunDriverCancel,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn submit_approval(
            &self,
            _approval: crate::HttpRunDriverApproval,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    write_production_test_config(&temp.path().join("sigil.toml"), ".");
    let attached_path = temp.path().join("attached-terminal-replay.jsonl");
    let foreign_path = temp.path().join("foreign-terminal-replay.jsonl");
    let (provider_name, route) = production_test_model_route(&temp);
    let attached_store = JsonlSessionStore::new(&attached_path)?;
    let attached_session =
        sigil_kernel::Session::new_with_route(provider_name, route).with_store(attached_store);
    let attached_scope_id = attached_session.session_scope_id().to_owned();
    JsonlSessionStore::new(&foreign_path)?;
    let registry = HttpSessionRunRegistry::new(Arc::new(MultipleBindingsDriver {
        bindings: Mutex::new(VecDeque::from([
            HttpSessionBinding {
                session_scope_id: attached_scope_id.clone(),
                session_log_path: canonical_http_session_path(&attached_path)?
                    .display()
                    .to_string(),
                route_transition: None,
                route_recovery: None,
            },
            HttpSessionBinding {
                session_scope_id: "foreign-durable-scope".to_owned(),
                session_log_path: canonical_http_session_path(&foreign_path)?
                    .display()
                    .to_string(),
                route_transition: None,
                route_recovery: None,
            },
        ])),
    }));
    let _attached_adapter_session = registry.create_session(HttpSessionCreateRequest::default())?;
    let foreign_adapter_session = registry.create_session(HttpSessionCreateRequest::default())?;
    let foreign_run = registry.start_run(
        &foreign_adapter_session.id,
        HttpRunStartRequest {
            prompt: "must remain owned by the foreign session".to_owned(),
            permission_mode: Some(HttpPermissionMode::Manual),
            model_ref: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
            task_continuation: None,
        },
    )?;
    registry.record_run_execution_uncertain(&foreign_run.id)?;

    let lifecycle = attached_session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
        &foreign_run.id,
        1,
    )?)?;
    let terminal = sigil_kernel::ConversationRunFinalizedEntryV1::new(
        &foreign_run.id,
        sigil_kernel::ConversationRunTerminalStatusV1::Failed,
        None,
        Some("durable log must not cross registry session ownership"),
        2,
        &sigil_kernel::SecretRedactor::empty(),
    )?;
    let event = PublicRunEvent::new(
        &attached_scope_id,
        &foreign_run.id,
        1,
        PublicRunEventKind::RunFailed {
            error: "durable log must not cross registry session ownership".to_owned(),
        },
    );
    let outbox = sigil_kernel::PublicEventOutboxEntryV1 {
        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!(
            "application-public:{attached_scope_id}:{}:1",
            foreign_run.id
        ),
        domain_event_id: format!("application-domain:{}:1", foreign_run.id),
        run_id: foreign_run.id.clone(),
        sequence: 1,
        payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&event)?),
        event,
    };
    lifecycle.append_finalized_with_outbox(&terminal, &outbox)?;

    let error = reconcile_registered_http_terminal_outboxes(
        &attached_path,
        &attached_scope_id,
        &registry,
        &HttpLiveEventBus::new(8),
    )
    .expect_err("cross-session run id must not be reconciled from this attachment");
    assert!(
        error
            .to_string()
            .contains("does not belong to the durable terminal outbox attachment")
    );
    assert_eq!(
        registry.get_run(&foreign_run.id)?.status,
        HttpRunStatus::ExecutionUncertain
    );
    Ok(())
}

#[tokio::test]
async fn production_cancel_returns_only_after_supervisor_acknowledges_activation() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(temp.path().join("sigil.toml"), temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let (cancel_sender, mut cancel_receiver) = mpsc::unbounded_channel();
    driver
        .active_runs
        .lock()
        .expect("active runs should lock")
        .insert(
            "run-1".to_owned(),
            Arc::new(HttpProductionActiveRun {
                session_id: "session-1".to_owned(),
                broker: Arc::new(HttpApprovalBroker::default()),
                cancel_sender,
                projection_owner: Arc::new(Mutex::new(None)),
            }),
        );
    let (finished, finished_rx) = std_mpsc::channel();
    let cancel_driver = Arc::clone(&driver);
    let caller = std::thread::spawn(move || {
        let result = cancel_driver.cancel_run(HttpRunDriverCancel {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            reason: Some("user requested stop".to_owned()),
        });
        finished
            .send(())
            .expect("completion signal should be delivered");
        result
    });
    let command = cancel_receiver
        .recv()
        .await
        .expect("supervisor should receive cancellation");
    let HttpProductionRunControlCommand::Cancel(command) = command else {
        panic!("cancel driver call must route a cancellation command");
    };

    assert_eq!(command.reason, "user requested stop");
    assert!(finished_rx.try_recv().is_err());
    drop(
        driver
            .active_runs
            .try_lock()
            .expect("waiting for cancel acknowledgement must not block natural run release"),
    );
    command
        .acknowledgement
        .send(Ok(()))
        .expect("durable activation acknowledgement should send");
    finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("driver call should finish after acknowledgement");
    caller
        .join()
        .expect("cancel caller should join")
        .expect("acknowledged cancellation should succeed");
}

#[tokio::test]
async fn production_task_pause_returns_only_after_supervisor_acknowledges_activation() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-pause.json"), 8)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(8, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures-pause.json"), 8)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(temp.path().join("sigil.toml"), temp.path()),
            disclosure_journal,
            event_bus,
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should accept a durable event bus"),
    );
    let (cancel_sender, mut cancel_receiver) = mpsc::unbounded_channel();
    driver
        .active_runs
        .lock()
        .expect("active runs should lock")
        .insert(
            "run-task-pause".to_owned(),
            Arc::new(HttpProductionActiveRun {
                session_id: "session-task-pause".to_owned(),
                broker: Arc::new(HttpApprovalBroker::default()),
                cancel_sender,
                projection_owner: Arc::new(Mutex::new(None)),
            }),
        );
    let request = TaskPauseRequest::new(
        TaskId::new("task-http-pause").expect("Task id should be valid"),
        3,
    );
    let expected = request.clone();
    let (finished, finished_rx) = std_mpsc::channel();
    let pause_driver = Arc::clone(&driver);
    let caller = std::thread::spawn(move || {
        let result = pause_driver.pause_task(crate::HttpRunDriverTaskPause {
            session_id: "session-task-pause".to_owned(),
            run_id: "run-task-pause".to_owned(),
            request,
        });
        finished
            .send(())
            .expect("completion signal should be delivered");
        result
    });
    let command = cancel_receiver
        .recv()
        .await
        .expect("supervisor should receive Task pause");
    let HttpProductionRunControlCommand::Pause(command) = command else {
        panic!("Task pause driver call must route a pause command");
    };

    assert_eq!(command.request, expected);
    assert!(finished_rx.try_recv().is_err());
    drop(
        driver
            .active_runs
            .try_lock()
            .expect("waiting for pause acknowledgement must not block natural run release"),
    );
    command
        .acknowledgement
        .send(Ok(()))
        .expect("durable pause acknowledgement should send");
    finished_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("driver call should finish after acknowledgement");
    caller
        .join()
        .expect("Task pause caller should join")
        .expect("acknowledged Task pause should succeed");
}

/// Seeds a durable session log with a committed typed draft bound to the current workspace
/// snapshot and returns the plan review id.
fn seed_revision_session(
    temp: &tempfile::TempDir,
    session: &HttpSessionSnapshot,
) -> sigil_kernel::PlanReviewId {
    let store = JsonlSessionStore::new(&session.session_log_path).expect("session store");
    let (provider_name, route) = production_test_model_route(temp);
    let mut session_log =
        sigil_kernel::Session::new_with_route(provider_name, route).with_store(store);
    session_log
        .ensure_identity_entry()
        .expect("durable session identity should append");
    let mut user_message = sigil_kernel::ModelMessage::user("design the coordinator migration");
    user_message.id = "message-1".to_owned();
    session_log
        .append_user_message(user_message)
        .expect("user message should append");
    let source = sigil_kernel::ConversationTurnRef::new(
        session_log.session_scope_id(),
        "message-1",
        "run-1",
    )
    .expect("turn ref should build");
    let decision = sigil_kernel::ConversationRouteDecisionRecordedEntry {
        decision_id: sigil_kernel::ConversationRouteDecisionId::new("decision-revision")
            .expect("decision id"),
        source_turn: source.clone(),
        route: sigil_kernel::ConversationRoute::PlanReview,
        reason_codes: vec![sigil_kernel::ConversationRouteReason::ArchitecturalTradeoff],
        configured_policy: sigil_kernel::TaskRoutingPolicy::Auto,
        effective_capability: sigil_kernel::AutomaticRouteCapability::ReviewFirst,
        policy_snapshot_hash: format!("sha256:{}", "a".repeat(64)),
        route_contract_fingerprint: format!("sha256:{}", "b".repeat(64)),
        decided_at_ms: 1,
    };
    let review_id = sigil_kernel::plan_review_id_for_source(&source);
    let decision_id = decision.decision_id.clone();
    session_log
        .append_control(sigil_kernel::ControlEntry::ConversationRouteDecisionRecorded(decision))
        .expect("route decision should append");
    let action = sigil_kernel::StartPlanReviewAction {
        decision_id,
        plan_review_id: review_id.clone(),
        plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
            &review_id,
            &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
        ),
        source_turn: source,
    };
    let request = sigil_runtime::PlanReviewCoordinator::prepare_automatic_plan_review(
        &mut session_log,
        &action,
        None,
        100,
    )
    .expect("automatic plan review should prepare");
    let mut handler = sigil_kernel::NoopEventHandler;
    sigil_runtime::PlanReviewCoordinator::ensure_attempt_started(
        &mut session_log,
        &request,
        &mut handler,
        105,
    )
    .expect("seed executor should mark its plan review attempt started");
    let draft = PlanDraftCreatedEntry {
        plan_id: request.plan_id.clone(),
        schema_version: 2,
        source: request.plan_source_ref(),
        plan_hash: format!("sha256:{}", "d".repeat(64)),
        summary: "Migrate the coordinator".to_owned(),
        inline_text: None,
        steps: vec![sigil_kernel::PlanDraftStep {
            step_id: "migrate_1".to_owned(),
            title: "Migrate coordinator".to_owned(),
            display_name: None,
            detail: None,
            role: Some(sigil_kernel::AgentRole::Executor),
            depends_on: Vec::new(),
            intent_aliases: Vec::new(),
            mode: Some(sigil_kernel::TaskStepMode::Write),
            isolation: Some(sigil_kernel::TaskIsolationMode::SequentialWorkspaceWrite),
            target_paths: vec!["src/coordinator.rs".to_owned()],
            required_capabilities: Vec::new(),
            deliverables: Vec::new(),
            acceptance_criteria: Vec::new(),
            suggested_checks: Vec::new(),
            risk: None,
            notes: Vec::new(),
        }],
        intent_proposal: None,
        target_paths: vec!["src/coordinator.rs".to_owned()],
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
        workspace_snapshot_id: request.workspace_snapshot_id.clone(),
        created_at_ms: 110,
    };
    sigil_runtime::PlanReviewCoordinator::commit_draft_from_child(
        &mut session_log,
        &draft,
        &request,
        &mut handler,
        120,
    )
    .expect("seed draft should commit");
    drop(session_log);
    review_id
}

fn production_revision_guidance_fixture(
    temp: &tempfile::TempDir,
    suffix: &str,
) -> (
    Arc<HttpProductionRunDriver>,
    Arc<HttpSessionRunRegistry>,
    HttpSessionSnapshot,
    HttpUserInputRequest,
) {
    let driver = production_queue_driver(temp, suffix);
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands.json"), 32)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("session should create");
    let review_id = seed_revision_session(temp, &session);
    let guidance = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "request-revision-guidance",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
                        &review_id,
                        &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
                    )
                    .as_str()
                    .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise should create durable guidance")
        .user_input_request
        .expect("Revise must return the exact guidance request");
    (driver, registry, session, guidance)
}

fn production_revision_guidance_answer() -> sigil_kernel::UserInputDecisionV1 {
    sigil_kernel::UserInputDecisionV1::Submitted {
        answers: vec![sigil_kernel::UserInputAnswerV1 {
            question_id: "revision_guidance".to_owned(),
            value: sigil_kernel::UserInputAnswerValueV1::Text {
                value: "Preserve the existing compatibility boundary.".to_owned(),
            },
        }],
    }
}

fn accept_production_revision_guidance(
    temp: &tempfile::TempDir,
    session: &HttpSessionSnapshot,
    guidance: &HttpUserInputRequest,
) -> sigil_runtime::PlanReviewRunRequest {
    let config = sigil_kernel::RootConfig::load(&temp.path().join("sigil.toml"))
        .expect("guidance should be accepted against valid configuration");
    let (_, request) = sigil_runtime::application_plan_revision_guidance_decision(
        &config,
        temp.path(),
        Path::new(&session.session_log_path),
        &session.durable_session_scope_id,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: guidance.identity.clone(),
            request_hash: guidance.request_hash.clone(),
            command_id: sigil_kernel::UserInputCommandId::new("accept-revision-guidance")
                .expect("guidance command id"),
            decision: production_revision_guidance_answer(),
        },
    )
    .expect("guidance should be accepted durably before dispatch");
    request.expect("accepted guidance must produce an executable revision request")
}

#[tokio::test]
async fn production_revision_config_failure_after_guidance_restores_base_plan_actions() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let (driver, registry, session, guidance) =
        production_revision_guidance_fixture(&temp, "revision-config-failure");
    let mutation = registry
        .reserve_durable_session_mutation(&session.durable_session_scope_id)
        .expect("guidance dispatch should own its mutation frontier");
    let request = accept_production_revision_guidance(&temp, &session, &guidance);
    let run_id = request.child_logical_run_id();
    let base_plan_id = request.base_plan_id.clone().expect("revision base Plan");

    // Exercise the real dispatch window after the runtime accepted guidance and before HTTP
    // reloads configuration. Corrupting it before acceptance would test an earlier safe failure.
    std::fs::write(temp.path().join("sigil.toml"), "invalid = [")
        .expect("configuration should become unreadable");
    let error = driver
        .spawn_plan_review_revision_from_current_config(&session, request)
        .expect_err("invalid current configuration must reject dispatch");
    assert!(error.message.contains("plan review config failed"));
    assert!(!error.message.contains("dispatch recovery failed"));
    drop(mutation);

    let log = sigil_kernel::Session::load_from_store_for_control(
        JsonlSessionStore::new(&session.session_log_path).expect("session store should reopen"),
    )
    .expect("failed dispatch should remain readable");
    assert_eq!(
        log.plan_artifact_projection()
            .latest_decision(&base_plan_id)
            .expect("revision failure should be durable")
            .decision,
        sigil_kernel::PlanDecision::RevisionFailed
    );
    assert_eq!(
        log.user_input_projection()
            .expect("input projection should read")
            .request(&guidance.identity)
            .expect("accepted input should remain durable")
            .public_view()
            .resolution,
        Some(sigil_kernel::UserInputResolutionV1::Consumed)
    );
    assert!(log.entries().iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if attempt.revision_request_id.as_ref() == Some(&guidance.identity.request_id)
    )));
    assert!(matches!(
        registry.get_run(&run_id),
        Err(HttpRegistryError::RunNotFound { .. })
    ));
    assert!(
        !driver
            .active_runs
            .lock()
            .expect("active-run state")
            .contains_key(&run_id)
    );

    registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "save-after-revision-dispatch-failure",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: base_plan_id.as_str().to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Save,
                    permission_grant: None,
                },
            ),
        )
        .expect("Save must remain available even while configuration is broken");
}

#[tokio::test]
async fn production_revision_config_failure_preserves_registered_queued_owner() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let (driver, registry, session, guidance) =
        production_revision_guidance_fixture(&temp, "revision-config-queued-owner");
    let mutation = registry
        .reserve_durable_session_mutation(&session.durable_session_scope_id)
        .expect("guidance dispatch should own its mutation frontier");
    let request = accept_production_revision_guidance(&temp, &session, &guidance);
    let run_id = request.child_logical_run_id();
    registry
        .register_supervised_session_run(
            &session.id,
            &run_id,
            HttpPermissionMode::ReadOnly,
            "queued revision owner",
        )
        .expect("the accepted revision should already have a registered owner");
    assert!(
        !driver
            .active_runs
            .lock()
            .expect("active-run state")
            .contains_key(&run_id)
    );
    let before = std::fs::read(&session.session_log_path).expect("accepted guidance should read");
    std::fs::write(temp.path().join("sigil.toml"), "invalid = [")
        .expect("configuration should become unreadable");

    let error = driver
        .spawn_plan_review_revision_from_current_config(&session, request)
        .expect_err("invalid current configuration must reject the duplicate caller");
    assert!(error.message.contains("plan review config failed"));
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("queued guidance should read"),
        before,
        "a registry owner without an active-run entry still prevents unstarted settlement"
    );
    assert_eq!(
        registry
            .get_run(&run_id)
            .expect("queued owner should remain")
            .status,
        HttpRunStatus::Running
    );
    registry.rollback_supervised_session_run_registration(&session.id, &run_id);
    drop(mutation);
}

#[tokio::test]
async fn production_revision_without_provider_publishes_failure_and_restores_base_plan_actions() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let (driver, registry, session, guidance) =
        production_revision_guidance_fixture(&temp, "revision-no-provider");
    let persisted_session = sigil_kernel::Session::load_from_store_for_control(
        JsonlSessionStore::new(&session.session_log_path).expect("session store should reopen"),
    )
    .expect("persisted revision route should remain readable");
    let persisted_route = persisted_session
        .resolved_model_route()
        .expect("the existing session must retain its selected route")
        .clone();
    drop(persisted_session);
    let config_path = temp.path().join("sigil.toml");
    let config = std::fs::read_to_string(&config_path).expect("configuration should read");
    std::fs::write(
        &config_path,
        config
            .split("[connections.local-test]")
            .next()
            .expect("config prefix"),
    )
    .expect("the configured provider connection should be removed");
    let disconnected_config = sigil_kernel::RootConfig::load(&config_path).expect("valid config");
    assert!(disconnected_config.connections.is_empty());
    let expected_route_error = sigil_runtime::provider_connections::validate_persisted_model_route(
        &disconnected_config,
        &persisted_route,
    )
    .expect_err("the persisted provider route must be unavailable")
    .to_string();
    let answer_registry = Arc::clone(&registry);
    let answer_session_id = session.id.clone();
    let answer_request_id = guidance.identity.request_id.as_str().to_owned();
    let receipt = tokio::task::spawn_blocking(move || {
        answer_registry.user_input_decision_command(
            &answer_session_id,
            &answer_request_id,
            HttpCommandEnvelope::new(
                "accept-revision-without-provider",
                "client-1",
                &answer_session_id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash,
                    decision: production_revision_guidance_answer(),
                    permission_mode: None,
                },
            ),
        )
    })
    .await
    .expect("guidance caller should join")
    .expect("guidance acceptance must not depend on provider assembly");
    let run_id = receipt
        .continuation_run_id
        .expect("revision should register a run");
    let idle_driver = Arc::clone(&driver);
    tokio::task::spawn_blocking(move || idle_driver.wait_for_idle(Duration::from_secs(10)))
        .await
        .expect("idle caller should join")
        .expect("provider resolution failure must release the owned revision run");
    let records = JsonlSessionStore::new(&session.session_log_path)
        .expect("session store should reopen")
        .read_event_records_writer()
        .expect("durable event records should read");
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("failed revision terminal should be consistent");
    let terminal_error = outbox
        .events_in_order()
        .into_iter()
        .find_map(|entry| match &entry.event.event {
            PublicRunEventKind::RunFailed { error } if entry.run_id == run_id => Some(error),
            _ => None,
        })
        .expect("the admitted revision must publish its provider failure");
    assert!(
        terminal_error.contains(&expected_route_error),
        "unexpected revision failure: {terminal_error}"
    );
    assert_eq!(
        registry
            .get_run(&run_id)
            .expect("failed run should remain visible")
            .status,
        HttpRunStatus::Failed
    );
    let log = sigil_kernel::Session::load_from_store_for_control(
        JsonlSessionStore::new(&session.session_log_path).expect("session store should reopen"),
    )
    .expect("failed revision should remain readable");
    let review = sigil_kernel::PlanReviewProjection::from_entries(log.entries());
    let attempt = review
        .reviews()
        .next()
        .expect("review should exist")
        .latest_attempt()
        .expect("revision attempt should exist");
    assert_eq!(
        attempt.status,
        sigil_kernel::PlanReviewAttemptStatus::Failed
    );
    let base_plan_id = attempt
        .base_plan_id
        .clone()
        .expect("revision base should remain");
    assert_eq!(
        log.plan_artifact_projection()
            .latest_decision(&base_plan_id)
            .expect("base decision")
            .decision,
        sigil_kernel::PlanDecision::RevisionFailed
    );
    registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "save-after-revision-provider-failure",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: base_plan_id.as_str().to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Save,
                    permission_grant: None,
                },
            ),
        )
        .expect("provider failure must restore Save on the original Plan");
}

#[tokio::test]
async fn production_revision_duplicate_registration_preserves_queued_plan_and_rejects_save() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");

    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "workspace"

{storage}

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:9"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("revision config should write");

    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-revision-dup.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("disclosures-revision-dup.json"),
            16,
        )
        .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands-revision-dup.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let review_id = seed_revision_session(&temp, &session);

    // Revise first creates durable guidance without dispatching a provider run.
    let attempt_1 = sigil_kernel::plan_review_attempt_id_for_review(&review_id);
    let guidance = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-guidance-request-1",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_1)
                        .as_str()
                        .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise should create durable guidance")
        .user_input_request
        .expect("Revise must return the exact guidance request");
    let attempt_2 = sigil_kernel::PlanReviewAttemptId::new(sigil_kernel::stable_event_uuid(
        "sigil-plan-review-revision-generation-attempt-v1",
        &format!(
            "{}|revision-request|{}|generation|{}|attempt|1",
            review_id.as_str(),
            guidance.identity.request_id.as_str(),
            guidance.identity.generation,
        ),
    ))
    .expect("revision dispatch identity should bind the accepted guidance generation");
    let revision_request_id = guidance.identity.request_id.clone();
    let revision_run_id = format!("plan-review-{}-{}", review_id.as_str(), attempt_2.as_str());
    driver
        .active_runs
        .lock()
        .expect("active-run state should not be poisoned")
        .insert(
            revision_run_id.clone(),
            Arc::new(HttpProductionActiveRun {
                session_id: session.id.clone(),
                broker: Arc::new(HttpApprovalBroker::default()),
                cancel_sender: mpsc::unbounded_channel().0,
                projection_owner: Arc::new(Mutex::new(None)),
            }),
        );

    // Pre-register the deterministic child run id so submitting guidance must reject the
    // duplicate only after the answer and RevisionRequested lineage are durable.
    let answer_registry = Arc::clone(&registry);
    let answer_session_id = session.id.clone();
    let answer_request_id = guidance.identity.request_id.as_str().to_owned();
    let error = tokio::task::spawn_blocking(move || {
        answer_registry.user_input_decision_command(
            &answer_session_id,
            &answer_request_id,
            HttpCommandEnvelope::new(
                "revision-dup-1",
                "client-1",
                &answer_session_id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash.clone(),
                    decision: sigil_kernel::UserInputDecisionV1::Submitted {
                        answers: vec![sigil_kernel::UserInputAnswerV1 {
                            question_id: "revision_guidance".to_owned(),
                            value: sigil_kernel::UserInputAnswerValueV1::Text {
                                value: "Preserve the existing compatibility boundary.".to_owned(),
                            },
                        }],
                    },
                    permission_mode: None,
                },
            ),
        )
    })
    .await
    .expect("user-input driver task should join")
    .expect_err("duplicate revision run id must be rejected before registration");
    assert!(
        error.to_string().contains("already active"),
        "rejection must name the duplicate run: {error}"
    );

    // The session is NOT blocked: the foreground slot was never claimed, so later durable
    // mutations remain possible despite the persisted RevisionRequested decision.
    drop(
        registry
            .reserve_durable_session_mutation(&session.durable_session_scope_id)
            .expect("session must remain mutable after a rejected revision registration"),
    );

    // The pre-existing owned run entry is untouched; rollback only removes what this call added.
    assert!(
        driver
            .active_runs
            .lock()
            .expect("active-run state should not be poisoned")
            .contains_key(&revision_run_id)
    );

    // The duplicate was rejected before the executor-owned `Started` fact. The durable guidance
    // remains a recovery candidate, rather than fabricating a failed domain terminal for an
    // adapter registration rejection.
    let store = JsonlSessionStore::new(&session.session_log_path).expect("reload store");
    let reloaded = sigil_kernel::Session::load_from_store("local-test", "gpt-4.1", store)
        .expect("revision session should reload");
    let original_plan_id = sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_1);
    let plan_projection = reloaded.plan_artifact_projection();
    let decision = plan_projection
        .latest_decision(&original_plan_id)
        .expect("durable revision guidance decision");
    assert_eq!(
        decision.decision,
        sigil_kernel::PlanDecision::RevisionRequested,
        "an adapter duplicate rejection must not overwrite the accepted durable guidance"
    );
    let review_projection = sigil_kernel::PlanReviewProjection::from_entries(reloaded.entries());
    assert!(
        review_projection
            .review(&review_id)
            .expect("base review should remain durable")
            .attempts
            .iter()
            .all(|attempt| attempt.revision_request_id.as_ref() != Some(&revision_request_id)),
        "a rejected duplicate must not invent a revision Started attempt"
    );
    let records = JsonlSessionStore::new(&session.session_log_path)
        .expect("reload durable records")
        .read_event_records_writer()
        .expect("durable recovery candidate records should read");
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("rejected duplicate must not leave a torn terminal outbox");
    assert!(
        outbox
            .events_in_order()
            .into_iter()
            .all(|entry| entry.run_id != revision_run_id),
        "a rejected duplicate must not emit a replacement terminal event"
    );

    // A duplicate owner is not proof of a failed dispatch. Keep the current queued revision
    // immutable until its actual owner settles; Save cannot abandon accepted guidance.
    let before = std::fs::read(&session.session_log_path).expect("read queued session bytes");
    let save_error = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-save-after-unstarted-duplicate",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: original_plan_id.as_str().to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Save,
                    permission_grant: None,
                },
            ),
        )
        .expect_err("a queued revision cannot be abandoned by Save");
    assert!(save_error.to_string().contains("unavailable"));
    assert_eq!(
        std::fs::read(&session.session_log_path).expect("read rejected Save bytes"),
        before
    );
    let reloaded = sigil_kernel::Session::load_from_store(
        "local-test",
        "gpt-4.1",
        JsonlSessionStore::new(&session.session_log_path).expect("reload saved session store"),
    )
    .expect("saved recovery session should reload");
    assert_eq!(
        reloaded
            .plan_artifact_projection()
            .latest_decision(&original_plan_id)
            .expect("save decision must be durable")
            .decision,
        sigil_kernel::PlanDecision::RevisionRequested,
        "the duplicate owner must settle before the original Plan becomes actionable"
    );
}

#[tokio::test]
async fn production_revision_bind_failure_rolls_back_only_the_run_registration() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");

    let config_path = temp.path().join("sigil.toml");
    let storage = isolated_storage_toml(&config_path);
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

{storage}

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:9"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("revision config should write");
    let root_config = sigil_kernel::RootConfig::load(&config_path).expect("config should load");
    let workspace_root = sigil_kernel::resolve_workspace_root(&config_path, temp.path(), ".");

    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-revision-bind.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("disclosures-revision-bind.json"),
            16,
        )
        .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands-revision-bind.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");

    // A different supervised run already owns the session foreground slot. Keep the setup on the
    // same atomic registry registration path as production rather than retaining the removed
    // slot-only helper.
    let mutation = registry
        .reserve_durable_session_mutation(&session.durable_session_scope_id)
        .expect("setup mutation reservation should be claimable");
    registry
        .register_supervised_session_run(
            &session.id,
            "other-foreground-run",
            HttpPermissionMode::ReadOnly,
            "other foreground run",
        )
        .expect("pre-existing foreground run should register");
    drop(mutation);
    let mut log = sigil_kernel::Session::new("local-test", "gpt-4.1");
    let request = sigil_runtime::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut log,
        "revise the plan",
        "run-1",
        None,
        1,
    )
    .expect("explicit revision request should prepare");
    let run_id = request.child_logical_run_id();

    let error = driver
        .spawn_plan_review_revision(&session, &root_config, &workspace_root, request)
        .expect_err("bind must fail while another run owns the foreground slot");
    assert!(
        error.to_string().contains("foreground"),
        "bind failure must name the slot conflict: {error}"
    );

    // The run-map entry this call inserted was rolled back...
    assert!(
        !driver
            .active_runs
            .lock()
            .expect("active-run state should not be poisoned")
            .contains_key(&run_id),
        "partial registration must be rolled back from active_runs"
    );
    // ...and the pre-existing slot owner is untouched: durable mutations remain blocked by the
    // OTHER run, not by a half-registered revision.
    let blocked = match registry.reserve_durable_session_mutation(&session.durable_session_scope_id)
    {
        Ok(_) => panic!("the other run must still own the foreground slot"),
        Err(error) => error,
    };
    assert!(
        blocked.to_string().contains("foreground"),
        "the pre-existing slot owner must still block mutations: {blocked}"
    );
}

#[tokio::test]
async fn production_plan_review_revision_runs_supervised_and_publishes_terminal_event() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    std::fs::write(
        workspace.join("README.md"),
        "coordinator migration fixture\n",
    )
    .expect("fixture workspace file should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let state_root = toml_path(&temp.path().join("state"));
    let cache_root = toml_path(&temp.path().join("cache"));

    // Local chat-completions fixture exercising the real current-schema review tool sequence.
    // The request always contains the available tool declarations, so selecting a response by
    // searching for `submit_plan_review_result` would make this test false-green.
    let provider_call = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address");
    let fixture = tokio::spawn({
        let provider_call = Arc::clone(&provider_call);
        async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = vec![0; 16384];
                let _read = socket.read(&mut buffer).await.unwrap_or(0);
                let call_index = provider_call.fetch_add(1, Ordering::SeqCst);
                let body = if call_index >= 3 {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-draft-call\",\"type\":\"function\",\"function\":{\"name\":\"submit_plan_review_result\",\"arguments\":\"{\\\"schema_version\\\":1,\\\"outcome\\\":\\\"draft\\\",\\\"content\\\":\\\"# Revised coordinator migration\\\\n\\\\n1. Revise migration\\\\n\\\\nPaths: src/coordinator.rs\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                } else if call_index == 2 {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-read-call\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                } else if call_index == 1 {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-grep-call\",\"type\":\"function\",\"function\":{\"name\":\"grep\",\"arguments\":\"{\\\"pattern\\\":\\\"coordinator\\\",\\\"path\\\":\\\".\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                } else {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-list-call\",\"type\":\"function\",\"function\":{\"name\":\"ls\",\"arguments\":\"{\\\"path\\\":\\\".\\\",\\\"limit\\\":20}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                };
                let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            }
        }
    });

    let base_url = format!("http://{address}");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[storage]
state_root = "{state_root}"
cache_root = "{cache_root}"

[workspace]
root = "workspace"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("revision config should write");

    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-revision.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures-revision.json"), 16)
            .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let command_store = Arc::new(
        HttpDurableCommandStore::open(temp.path().join("commands-revision.json"), 32)
            .expect("command store should initialize"),
    );
    let registry = driver
        .build_registry(command_store)
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let mut subscriber = event_bus.subscribe();

    let review_id = seed_revision_session(&temp, &session);

    // Revise through the registry: the driver runs a supervised revision, publishes an explicit
    // terminal event, closes the SSE stream, and releases the session foreground slot.
    let plan_receipt = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-command-1",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
                        &review_id,
                        &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
                    )
                    .as_str()
                    .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise decision should be accepted");
    assert_eq!(plan_receipt.action, HttpPlanDecisionAction::Revise);
    assert!(plan_receipt.revision_run_id.is_none());
    let guidance = plan_receipt
        .user_input_request
        .expect("Revise must expose durable guidance before dispatch");
    let answer_registry = Arc::clone(&registry);
    let answer_session_id = session.id.clone();
    let answer_request_id = guidance.identity.request_id.as_str().to_owned();
    let answer_request_id_for_command = answer_request_id.clone();
    let answer_receipt = tokio::task::spawn_blocking(move || {
        answer_registry.user_input_decision_command(
            &answer_session_id,
            &answer_request_id_for_command,
            HttpCommandEnvelope::new(
                "revision-guidance-answer-1",
                "client-1",
                &answer_session_id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash.clone(),
                    decision: sigil_kernel::UserInputDecisionV1::Submitted {
                        answers: vec![sigil_kernel::UserInputAnswerV1 {
                            question_id: "revision_guidance".to_owned(),
                            value: sigil_kernel::UserInputAnswerValueV1::Text {
                                value: "Keep the public contract stable.".to_owned(),
                            },
                        }],
                    },
                    permission_mode: None,
                },
            ),
        )
    })
    .await
    .expect("user-input driver task should join")
    .expect("revision guidance should start a supervised revision");
    let revision_run_id = answer_receipt
        .continuation_run_id
        .expect("guidance receipt must expose the revision run identity");

    // The supervised revision owns the session foreground slot while it runs.
    assert!(
        registry
            .reserve_durable_session_mutation(&session.durable_session_scope_id)
            .is_err(),
        "concurrent durable mutations must be blocked while the revision runs"
    );

    // The run publishes a terminal event and closes the SSE stream for its run identity.
    let (terminal_event, closed) = tokio::time::timeout(Duration::from_secs(15), async {
        let mut terminal = None;
        let mut closed = None;
        loop {
            match subscriber.recv_run_stream().await.expect("live event") {
                crate::sse::HttpRunStreamReceive::Event(event) => {
                    if event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .run_id
                        == revision_run_id
                        && matches!(
                            &event
                                .run_event
                                .as_ref()
                                .expect("public event payload")
                                .event,
                            PublicRunEventKind::RunFinished { .. }
                        )
                    {
                        terminal = Some(event);
                    }
                }
                crate::sse::HttpRunStreamReceive::StreamClosed { session_id, run_id }
                    if run_id == revision_run_id =>
                {
                    closed = Some((session_id, run_id));
                }
                crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {}
            }
            if terminal.is_some() && closed.is_some() {
                break (terminal, closed);
            }
        }
    })
    .await
    .expect("revision should publish a terminal event and close its stream");
    let (terminal, (closed_session, closed_run)) = terminal_event
        .zip(closed)
        .expect("revision terminal event and stream close");
    assert!(matches!(
        &terminal
            .run_event
            .as_ref()
            .expect("public event payload")
            .event,
        PublicRunEventKind::RunFinished { .. }
    ));
    assert_eq!(closed_session, session.durable_session_scope_id);
    assert_eq!(closed_run, revision_run_id);
    assert!(
        terminal
            .run_event
            .as_ref()
            .expect("public event payload")
            .sequence
            > 1,
        "the revision terminal must retain the bridge sequence after preceding live events"
    );
    assert_eq!(
        registry
            .get_run(&revision_run_id)
            .expect("revision child must remain registered through terminal projection")
            .status,
        HttpRunStatus::Finished,
        "the registered revision child must reconcile its durable terminal outcome"
    );

    // The durable session now carries the revised draft and the DraftReady attempt.
    let store = JsonlSessionStore::new(&session.session_log_path).expect("reload store");
    let reloaded = sigil_kernel::Session::load_from_store("local-test", "gpt-4.1", store)
        .expect("revision session should reload");
    let projection = sigil_kernel::PlanReviewProjection::from_entries(reloaded.entries());
    assert!(!projection.has_conflicts());
    assert_eq!(
        projection
            .latest_attempt(&review_id)
            .map(|attempt| attempt.status)
            .expect("revision attempt"),
        sigil_kernel::PlanReviewAttemptStatus::DraftReady
    );
    assert!(
        reloaded
            .plan_artifact_projection()
            .plans
            .values()
            .any(|draft| draft.summary == "Revised coordinator migration")
    );
    let records = JsonlSessionStore::new(&session.session_log_path)
        .expect("reload durable records")
        .read_event_records_writer()
        .expect("read revision terminal bundle");
    let outbox_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("revision terminal bundle must be valid");
    let outbox = outbox_projection
        .events_in_order()
        .into_iter()
        .find(|entry| {
            entry.run_id == revision_run_id
                && entry.sequence
                    == terminal
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .sequence
                && matches!(&entry.event.event, PublicRunEventKind::RunFinished { .. })
        })
        .expect("revision must persist exactly one terminal outbox");
    assert_eq!(
        serde_json::to_value(&outbox.event).expect("terminal outbox should serialize"),
        serde_json::to_value(&terminal.run_event).expect("terminal SSE event should serialize")
    );
    assert!(
        !outbox_projection
            .pending_for_adapter("http")
            .into_iter()
            .any(|entry| entry.public_event_id == outbox.public_event_id),
        "the HTTP receipt must acknowledge the same durable terminal payload"
    );
    let revision_request_id = sigil_kernel::UserInputRequestId::new(answer_request_id.clone())
        .expect("revision guidance request id should remain valid");
    let attempt = projection
        .latest_attempt(&review_id)
        .expect("durable revision attempt");
    assert_eq!(
        attempt.revision_request_id.as_ref(),
        Some(&revision_request_id)
    );
    assert_eq!(attempt.attempt_ordinal, 1);
    let attempt_id = &attempt.attempt_id;
    let child_key = format!("pr-{}-research-0", attempt_id.as_str());
    let child_scope_id = format!("{}-research", revision_run_id);
    let child_session_log_path = driver
        .services
        .authority_composition()
        .expect("current-schema authority composition should be present")
        .storage_writer
        .managed_named_leaf_path(
            sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
            &child_key,
        )
        .expect("child session namespace should resolve")
        .join("records.jsonl");
    let child_entries = JsonlSessionStore::read_entries(&child_session_log_path)
        .expect("the production child durable log should remain readable after settlement");
    assert!(
        !child_entries.is_empty(),
        "the production child durable log must contain the supervised tool results"
    );
    let mut durable_tool_names = BTreeSet::new();
    let mut readable_tool_names = BTreeSet::new();
    for entry in &child_entries {
        let SessionLogEntry::ToolResultV3(result) = entry else {
            continue;
        };
        let descriptor = result
            .artifact
            .descriptor()
            .expect("every current-schema Revise tool result must publish a durable artifact");
        descriptor
            .validate()
            .expect("Revise artifact descriptor must validate");
        durable_tool_names.insert(result.tool_name.clone());
        let page = driver
            .read_managed_plan_review_child_artifact(
                &child_key,
                &child_scope_id,
                &child_session_log_path,
                &descriptor.artifact_ref,
                sigil_kernel::session::ToolArtifactSelectorV1::ByteSlice {
                    offset: 0,
                    limit: descriptor.persisted_bytes.clamp(1, 1024) as u32,
                },
            )
            .expect("Revise artifact descriptor must be readable through HTTP authority");
        assert_eq!(page.artifact_ref, descriptor.artifact_ref);
        assert_eq!(
            result.facts.status, "ok",
            "child durable result must prove the tool completed successfully: tool={} error={:?} details={:?}",
            result.tool_name, result.facts.error, result.facts.tool_specific
        );
        if matches!(result.tool_name.as_str(), "ls" | "grep" | "read_file") {
            let receipt = result
                .facts
                .tool_specific
                .get("managed_access_receipt")
                .and_then(serde_json::Value::as_object)
                .expect("managed file result must persist its authority access receipt");
            assert!(
                receipt
                    .get("receipt_hash")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|hash| !hash.is_empty()),
                "managed access receipt must have a durable receipt hash"
            );
            readable_tool_names.insert(result.tool_name.clone());
        }
    }
    assert_eq!(
        readable_tool_names,
        BTreeSet::from(["grep".to_owned(), "ls".to_owned(), "read_file".to_owned(),]),
        "the three current-schema inspection tools must all succeed and publish artifacts; durable={durable_tool_names:?}"
    );
    assert!(
        durable_tool_names.contains("submit_plan_review_result"),
        "submit_plan_review_result must also leave a durable artifact-backed tool result"
    );
    assert!(
        provider_call.load(Ordering::SeqCst) >= 4,
        "revision must execute ls, grep, read_file, and submit_plan_review_result"
    );

    // Ownership is released: the run leaves active_runs and the foreground slot is free.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let runs = driver
                    .active_runs
                    .lock()
                    .expect("active-run state should not be poisoned");
                if runs.is_empty() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("revision should release itself from active_runs");
    let _mutation = registry
        .reserve_durable_session_mutation(&session.durable_session_scope_id)
        .expect("foreground slot must be released after the revision");
    fixture.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_review_revision_cooperative_cancellation_commits_one_exact_terminal() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let state_root = toml_path(&temp.path().join("state"));
    let cache_root = toml_path(&temp.path().join("cache"));

    // Keep the real provider request pending until cancellation reaches the provider-stream
    // select. The production cancellation handle is expected to end that stream cooperatively;
    // deadline ownership is exercised separately with a controlled supervisor future below.
    let provider_started = Arc::new(tokio::sync::Semaphore::new(0));
    let provider_release = Arc::new(tokio::sync::Semaphore::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address");
    let fixture = tokio::spawn({
        let provider_started = Arc::clone(&provider_started);
        let provider_release = Arc::clone(&provider_release);
        async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .expect("provider request should arrive");
            let mut buffer = vec![0; 16 * 1024];
            let _ = socket.read(&mut buffer).await;
            provider_started.add_permits(1);
            provider_release
                .acquire()
                .await
                .expect("fixture release should remain available")
                .forget();
            let _ = socket.shutdown().await;
        }
    });

    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[storage]
state_root = "{state_root}"
cache_root = "{cache_root}"

[workspace]
root = "workspace"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://{address}"
credential = {{ source = "none" }}
"#,
        ),
    )
    .expect("revision config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-revision-deadline.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(16, protocol_journal));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("disclosures-revision-deadline.json"),
            16,
        )
        .expect("disclosure journal should initialize"),
    );
    let mut options = HttpProductionRunDriverOptions::new(&config_path, temp.path());
    // This case exercises cooperative cancellation; the separate controlled-future test
    // deterministically exercises the deadline branch without racing filesystem latency.
    options.cancellation_timeout = Duration::from_secs(5);
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            options,
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-revision-deadline.json"), 32)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let review_id = seed_revision_session(&temp, &session);

    let guidance = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-deadline-command",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
                        &review_id,
                        &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
                    )
                    .as_str()
                    .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise decision should expose guidance")
        .user_input_request
        .expect("revision guidance should be durable");
    let guidance_receipt = registry
        .user_input_decision_command(
            &session.id,
            guidance.identity.request_id.as_str(),
            HttpCommandEnvelope::new(
                "revision-deadline-guidance-answer",
                "client-1",
                &session.id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash,
                    decision: sigil_kernel::UserInputDecisionV1::Submitted {
                        answers: vec![sigil_kernel::UserInputAnswerV1 {
                            question_id: "revision_guidance".to_owned(),
                            value: sigil_kernel::UserInputAnswerValueV1::Text {
                                value: "Keep the public contract stable.".to_owned(),
                            },
                        }],
                    },
                    permission_mode: None,
                },
            ),
        )
        .expect("guidance answer should start the revision");
    let revision_run_id = guidance_receipt
        .continuation_run_id
        .expect("guidance receipt should expose the revision child run");
    tokio::time::timeout(Duration::from_secs(10), provider_started.acquire())
        .await
        .expect("revision should reach its owned provider request within the fixture deadline")
        .expect("provider-start signal should remain available")
        .forget();

    let cancel_registry = Arc::clone(&registry);
    let cancel_run_id = revision_run_id.clone();
    let cancellation = tokio::time::timeout(
        // The provider remains held until cancellation returns. This outer guard only detects
        // a stalled command; it is deliberately larger than the configured driver deadline.
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || cancel_registry.cancel_run(&cancel_run_id)),
    )
    .await
    .expect("revision cancellation caller must return at its deadline")
    .expect("revision cancellation worker should join");
    assert!(
        matches!(
            &cancellation,
            Ok(snapshot)
                if matches!(
                    snapshot.status,
                    HttpRunStatus::CancelRequested | HttpRunStatus::Cancelled
                )
        ),
        "the real provider stream must acknowledge cooperative cancellation, not report a deadline rejection: {cancellation:?}"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let settled = registry
                .get_run(&revision_run_id)
                .expect("revision checkpoint should remain visible")
                .status
                == HttpRunStatus::Cancelled;
            if settled
                && driver
                    .active_run_count()
                    .expect("active owner should remain observable")
                    == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cooperatively cancelled revision should release its owner");
    let _mutation = registry
        .reserve_durable_session_mutation(&session.durable_session_scope_id)
        .expect("cooperatively cancelled revision must release its session attachment");
    let records = JsonlSessionStore::read_event_records(&session.session_log_path)
        .expect("cancelled revision records should remain readable");
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("cancelled revision must retain its strict durable terminal pair");
    let terminals = outbox
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            entry.run_id == revision_run_id
                && matches!(
                    &entry.event.event,
                    PublicRunEventKind::RunFinished { .. }
                        | PublicRunEventKind::RunFailed { .. }
                        | PublicRunEventKind::RunCancelled
                        | PublicRunEventKind::RunInterrupted { .. }
                        | PublicRunEventKind::RunPaused { .. }
                        | PublicRunEventKind::RunBlocked { .. }
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        terminals.len(),
        1,
        "cancellation must not append a replacement terminal"
    );
    assert!(matches!(
        terminals[0].event.event,
        PublicRunEventKind::RunCancelled
    ));
    assert!(
        !outbox
            .pending_for_adapter("http")
            .into_iter()
            .any(|entry| entry.public_event_id == terminals[0].public_event_id),
        "the exact cancellation event must receive its one HTTP receipt"
    );
    assert_eq!(
        event_bus
            .replay_run_after(&session.durable_session_scope_id, &revision_run_id, None)
            .expect("cancelled revision must retain its canonical HTTP event")
            .into_iter()
            .filter(|event| {
                http_terminal_from_durable_public_event(
                    &event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .event,
                )
                .is_some()
            })
            .map(|event| {
                serde_json::to_value(&event.run_event)
                    .expect("published revision terminal should serialize")
            })
            .collect::<Vec<_>>(),
        vec![
            serde_json::to_value(&terminals[0].event)
                .expect("durable revision terminal should serialize")
        ],
        "cooperative cancellation must publish its original terminal once"
    );

    provider_release.add_permits(1);
    fixture
        .await
        .expect("provider fixture should exit after release");
}

#[tokio::test]
async fn plan_review_revision_cancellation_deadline_transfers_real_attachment_to_late_owner()
-> anyhow::Result<()> {
    struct RegistryOnlyDriver {
        binding: HttpSessionBinding,
    }

    impl HttpRunDriver for RegistryOnlyDriver {
        fn bind_session(
            &self,
            _session_id: &str,
            _model_ref: Option<&crate::HttpProviderModelRef>,
        ) -> Result<HttpSessionBinding, crate::HttpRunDriverError> {
            Ok(self.binding.clone())
        }

        fn start_run(
            &self,
            _start: crate::HttpRunDriverStart,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn cancel_run(
            &self,
            _cancel: crate::HttpRunDriverCancel,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }

        fn submit_approval(
            &self,
            _approval: crate::HttpRunDriverApproval,
        ) -> Result<(), crate::HttpRunDriverError> {
            Ok(())
        }
    }

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("revision-deadline.jsonl");
    std::fs::write(&session_path, "")?;
    let durable_session_scope_id = "revision-deadline-scope".to_owned();
    let registry = HttpSessionRunRegistry::new(Arc::new(RegistryOnlyDriver {
        binding: HttpSessionBinding {
            session_scope_id: durable_session_scope_id.clone(),
            session_log_path: canonical_http_session_path(&session_path)?
                .display()
                .to_string(),
            route_transition: None,
            route_recovery: None,
        },
    }));
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let run_id = "plan-review-revision-deadline";
    let mutation = registry.reserve_durable_session_mutation(&durable_session_scope_id)?;
    registry.register_or_resume_supervised_revision_run(
        &session.id,
        run_id,
        HttpPermissionMode::ReadOnly,
        "revision deadline fixture",
        false,
    )?;
    drop(mutation);

    let attachment = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_path,
        )?,
    );
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let run: PlanReviewRevisionExecutionFuture = Box::pin({
        let release = Arc::clone(&release);
        async move {
            release
                .acquire()
                .await
                .expect("late revision fixture release must remain available")
                .forget();
            Err(anyhow::anyhow!(
                "controlled late revision fixture ended after attachment release"
            ))
        }
    });
    let cancellation_owner = sigil_kernel::RunCancellationOwner::new();
    let (acknowledgement, acknowledged) = std::sync::mpsc::sync_channel(1);
    let detached = match await_plan_review_revision_cancellation(
        &cancellation_owner,
        HttpProductionCancellationCommand {
            reason: "fixture cancellation".to_owned(),
            acknowledgement,
        },
        Duration::from_millis(10),
        run,
        attachment,
        Some(&registry),
        run_id,
    )
    .await
    {
        PlanReviewRevisionCancellationWait::Deadline(detached) => detached,
        PlanReviewRevisionCancellationWait::Joined { .. } => {
            panic!("controlled pending revision must transfer to a late owner at the deadline")
        }
    };
    let acknowledgement = acknowledged
        .recv_timeout(Duration::from_secs(1))
        .expect("deadline acknowledgement must be bounded")
        .expect_err("deadline acknowledgement must reject the HTTP cancel");
    assert!(
        acknowledgement
            .message
            .contains("did not quiesce before the cancellation deadline")
    );
    assert!(cancellation_owner.handle().is_cancel_requested());
    assert_eq!(
        registry.get_run(run_id)?.status,
        HttpRunStatus::ExecutionUncertain,
        "the deadline must project uncertainty before the late owner is acknowledged"
    );
    assert!(matches!(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_path
        ),
        Err(sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::Busy { .. })
    ));

    release.add_permits(1);
    let (late_run, attachment) = detached.into_parts();
    let late_result = late_run.await;
    assert!(
        late_result.is_err(),
        "the controlled late future must not forge a terminal"
    );
    drop(attachment);
    let recovered =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_path,
        )?;
    drop(recovered);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_plan_review_waiting_input_resumes_same_run_without_a_terminal_outbox() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let state_root = toml_path(&temp.path().join("state"));
    let cache_root = toml_path(&temp.path().join("cache"));

    let provider_call = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address");
    let fixture = tokio::spawn({
        let provider_call = Arc::clone(&provider_call);
        async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = vec![0; 16384];
                let _read = socket.read(&mut buffer).await.unwrap_or(0);
                let call_index = provider_call.fetch_add(1, Ordering::SeqCst);
                let body = if call_index == 0 {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-question-call\",\"type\":\"function\",\"function\":{\"name\":\"request_user_input\",\"arguments\":\"{\\\"prompt\\\":\\\"Choose the migration boundary\\\",\\\"questions\\\":[{\\\"id\\\":\\\"scope\\\",\\\"header\\\":\\\"Scope\\\",\\\"question\\\":\\\"Which module should be migrated first?\\\",\\\"required\\\":true,\\\"field\\\":{\\\"kind\\\":\\\"text\\\",\\\"multiline\\\":false,\\\"max_chars\\\":120}}]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                } else {
                    concat!(
                        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-draft-after-answer\",\"type\":\"function\",\"function\":{\"name\":\"submit_plan_review_result\",\"arguments\":\"{\\\"schema_version\\\":1,\\\"outcome\\\":\\\"draft\\\",\\\"content\\\":\\\"# Revised after research answer\\\\n\\\\n1. Revise migration\\\\n\\\\nPaths: src/coordinator.rs\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )
                };
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        }
    });

    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[storage]
state_root = "{state_root}"
cache_root = "{cache_root}"

[workspace]
root = "workspace"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://{address}"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("revision config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-revision-waiting.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        16,
        Arc::clone(&protocol_journal),
    ));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("disclosures-revision-waiting.json"),
            16,
        )
        .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-revision-waiting.json"), 32)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let review_id = seed_revision_session(&temp, &session);
    let mut subscriber = event_bus.subscribe();

    let guidance = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-waiting-command-1",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
                        &review_id,
                        &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
                    )
                    .as_str()
                    .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise decision should expose guidance")
        .user_input_request
        .expect("revision guidance request should be durable");
    let guidance_receipt = registry
        .user_input_decision_command(
            &session.id,
            guidance.identity.request_id.as_str(),
            HttpCommandEnvelope::new(
                "revision-waiting-guidance-answer",
                "client-1",
                &session.id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash.clone(),
                    decision: sigil_kernel::UserInputDecisionV1::Submitted {
                        answers: vec![sigil_kernel::UserInputAnswerV1 {
                            question_id: "revision_guidance".to_owned(),
                            value: sigil_kernel::UserInputAnswerValueV1::Text {
                                value: "Ask for the migration scope first.".to_owned(),
                            },
                        }],
                    },
                    permission_mode: None,
                },
            ),
        )
        .expect("guidance answer should start the revision");
    let revision_run_id = guidance_receipt
        .continuation_run_id
        .expect("guidance answer should expose the revision child run");

    let waiting_event = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match subscriber
                .recv_run_stream()
                .await
                .expect("waiting live event")
            {
                crate::sse::HttpRunStreamReceive::Event(event)
                    if event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .run_id
                        == revision_run_id
                        && matches!(
                            event
                                .run_event
                                .as_ref()
                                .expect("public event payload")
                                .event,
                            PublicRunEventKind::RunAwaitingUserInput { .. }
                        ) =>
                {
                    break event.run_event.expect("filtered public payload");
                }
                crate::sse::HttpRunStreamReceive::Event(_)
                | crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {}
            }
        }
    })
    .await
    .expect("revision should publish its durable waiting input event");
    let PublicRunEventKind::RunAwaitingUserInput {
        request_id,
        generation,
        request_hash,
    } = waiting_event.event
    else {
        panic!("waiting event kind was filtered above");
    };
    let waiting_sequence = waiting_event.sequence;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if registry
                .get_run(&revision_run_id)
                .expect("revision child should be registered")
                .status
                == HttpRunStatus::Paused
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiting attempt should project to a resumable registry pause");
    assert!(
        driver
            .has_unresolved_user_input(&session)
            .expect("current waiting review must block unrelated foreground input")
    );
    let records = JsonlSessionStore::new(&session.session_log_path)
        .expect("session store")
        .read_event_records_writer()
        .expect("session records");
    let waiting_projection = sigil_kernel::PlanReviewProjection::from_entries(
        &sigil_kernel::JsonlSessionStore::read_entries(&session.session_log_path)
            .expect("waiting entries"),
    );
    let waiting_attempt = waiting_projection
        .latest_attempt(&review_id)
        .expect("waiting revision attempt");
    let unmanaged_child_path = waiting_attempt.child_session_ref.resolve(
        Path::new(&session.session_log_path)
            .parent()
            .expect("parent session directory"),
    );
    assert!(
        !unmanaged_child_path.exists(),
        "managed research must not create the parent-relative child log"
    );
    assert_eq!(
        waiting_attempt.status,
        sigil_kernel::PlanReviewAttemptStatus::WaitingForInput
    );
    let outbox_before_resume = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("waiting records should not contain a terminal outbox tear");
    let revision_outboxes_before_resume = outbox_before_resume
        .events_in_order()
        .into_iter()
        .filter(|entry| entry.run_id == revision_run_id)
        .collect::<Vec<_>>();
    assert!(
        revision_outboxes_before_resume.iter().any(|entry| {
            matches!(
                &entry.event.event,
                PublicRunEventKind::RunAwaitingUserInput { .. }
            )
        }),
        "WaitingForInput must persist its durable public checkpoint outbox"
    );
    assert!(
        revision_outboxes_before_resume.iter().all(|entry| {
            !matches!(
                &entry.event.event,
                PublicRunEventKind::RunFinished { .. }
                    | PublicRunEventKind::RunFailed { .. }
                    | PublicRunEventKind::RunCancelled
                    | PublicRunEventKind::RunInterrupted { .. }
                    | PublicRunEventKind::RunPaused { .. }
                    | PublicRunEventKind::RunBlocked { .. }
            )
        }),
        "WaitingForInput must not create a final revision terminal outbox"
    );

    let resumed = registry
        .user_input_decision_command(
            &session.id,
            &request_id,
            HttpCommandEnvelope::new(
                "revision-waiting-research-answer",
                "client-1",
                &session.id,
                HttpUserInputDecisionRequest {
                    generation,
                    expected_request_hash: request_hash,
                    decision: sigil_kernel::UserInputDecisionV1::Submitted {
                        answers: vec![sigil_kernel::UserInputAnswerV1 {
                            question_id: "scope".to_owned(),
                            value: sigil_kernel::UserInputAnswerValueV1::Text {
                                value: "crates/sigil-kernel".to_owned(),
                            },
                        }],
                    },
                    permission_mode: None,
                },
            ),
        )
        .expect("the exact research answer should resume the same revision attempt");
    assert_eq!(
        resumed.continuation_run_id.as_deref(),
        Some(revision_run_id.as_str())
    );

    let terminal_event = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match subscriber
                .recv_run_stream()
                .await
                .expect("revision terminal live event")
            {
                crate::sse::HttpRunStreamReceive::Event(event)
                    if event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .run_id
                        == revision_run_id
                        && matches!(
                            event
                                .run_event
                                .as_ref()
                                .expect("public event payload")
                                .event,
                            PublicRunEventKind::RunFinished { .. }
                        ) =>
                {
                    break event.run_event.expect("filtered public payload");
                }
                crate::sse::HttpRunStreamReceive::Event(_)
                | crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {}
            }
        }
    })
    .await
    .expect("resumed revision should publish its terminal event");
    assert!(
        terminal_event.sequence > waiting_sequence,
        "same revision child must continue its exact public sequence after waiting"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if registry
                .get_run(&revision_run_id)
                .expect("revision child should remain registered")
                .status
                == HttpRunStatus::Finished
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("durable terminal delivery must reconcile the registry to Finished");
    assert_eq!(
        registry
            .get_run(&revision_run_id)
            .expect("revision child should remain registered")
            .status,
        HttpRunStatus::Finished
    );
    assert!(
        !driver
            .has_unresolved_user_input(&session)
            .expect("terminal revision must clear only its current waiting admission")
    );
    let records = JsonlSessionStore::new(&session.session_log_path)
        .expect("session store")
        .read_event_records_writer()
        .expect("terminal records");
    let outbox_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("terminal bundle should validate");
    let terminal_outboxes = outbox_projection
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            entry.run_id == revision_run_id
                && entry.sequence == terminal_event.sequence
                && matches!(&entry.event.event, PublicRunEventKind::RunFinished { .. })
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_outboxes.len(), 1);
    assert_eq!(
        serde_json::to_value(&terminal_outboxes[0].event)
            .expect("durable terminal event should serialize"),
        serde_json::to_value(&terminal_event).expect("live terminal event should serialize")
    );
    assert!(
        !outbox_projection
            .pending_for_adapter("http")
            .into_iter()
            .any(|entry| entry.public_event_id == terminal_outboxes[0].public_event_id),
        "only the final durable terminal receives an HTTP delivery receipt"
    );
    assert!(
        !unmanaged_child_path.exists(),
        "answer and resumed execution must reopen the original managed child, not a parallel log"
    );
    driver
        .wait_for_idle(Duration::from_secs(10))
        .expect("resumed revision should release its active owner");
    fixture.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_plan_review_waiting_cancel_uses_the_next_exact_outbox_sequence() {
    production_plan_review_waiting_cancel_scenario(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_plan_review_waiting_cancel_recovers_accepted_child_after_cached_failure() {
    production_plan_review_waiting_cancel_scenario(true).await;
}

async fn production_plan_review_waiting_cancel_scenario(recover_accepted_child: bool) {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");
    let state_root = toml_path(&temp.path().join("state"));
    let cache_root = toml_path(&temp.path().join("cache"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener should bind");
    let address = listener.local_addr().expect("fixture address");
    let fixture = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = vec![0; 16384];
            let _read = socket.read(&mut buffer).await.unwrap_or(0);
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-cancel-question\",\"type\":\"function\",\"function\":{\"name\":\"request_user_input\",\"arguments\":\"{\\\"prompt\\\":\\\"Choose the migration boundary\\\",\\\"questions\\\":[{\\\"id\\\":\\\"scope\\\",\\\"header\\\":\\\"Scope\\\",\\\"question\\\":\\\"Which module should be migrated first?\\\",\\\"required\\\":true,\\\"field\\\":{\\\"kind\\\":\\\"text\\\",\\\"multiline\\\":false,\\\"max_chars\\\":120}}]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
        }
    });

    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[storage]
state_root = "{state_root}"
cache_root = "{cache_root}"

[workspace]
root = "workspace"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[task]
routing_policy = "auto"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://{address}"
credential = {{ source = "none" }}
"#
        ),
    )
    .expect("revision config should write");
    let protocol_journal = Arc::new(
        HttpDurableProtocolJournal::open(temp.path().join("protocol-revision-cancel.json"), 32)
            .expect("protocol journal should initialize"),
    );
    let event_bus = Arc::new(HttpLiveEventBus::with_durable_journal(
        16,
        Arc::clone(&protocol_journal),
    ));
    let disclosure_journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("disclosures-revision-cancel.json"),
            16,
        )
        .expect("disclosure journal should initialize"),
    );
    let driver = Arc::new(
        HttpProductionRunDriver::new(
            HttpProductionRunDriverOptions::new(&config_path, temp.path()).with_session_lifecycle(
                sigil_runtime::LocalSessionLifecycleService::new(
                    "revision-cancel-recovery",
                    &sessions,
                    temp.path().join("session-exports"),
                ),
            ),
            disclosure_journal,
            Arc::clone(&event_bus),
            tokio::runtime::Handle::current(),
        )
        .expect("production driver should initialize"),
    );
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands-revision-cancel.json"), 32)
                .expect("command store should initialize"),
        ))
        .expect("production registry should attach");
    let session = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("durable session binding should not require provider assembly");
    let review_id = seed_revision_session(&temp, &session);
    let mut subscriber = event_bus.subscribe();

    let guidance = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-cancel-command-1",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(
                        &review_id,
                        &sigil_kernel::plan_review_attempt_id_for_review(&review_id),
                    )
                    .as_str()
                    .to_owned(),
                    expected_plan_hash: format!("sha256:{}", "d".repeat(64)),
                    expected_candidate_hash: None,
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise decision should expose guidance")
        .user_input_request
        .expect("revision guidance request should be durable");
    let guidance_receipt = registry
        .user_input_decision_command(
            &session.id,
            guidance.identity.request_id.as_str(),
            HttpCommandEnvelope::new(
                "revision-cancel-guidance-answer",
                "client-1",
                &session.id,
                HttpUserInputDecisionRequest {
                    generation: guidance.identity.generation,
                    expected_request_hash: guidance.request_hash.clone(),
                    decision: sigil_kernel::UserInputDecisionV1::Submitted {
                        answers: vec![sigil_kernel::UserInputAnswerV1 {
                            question_id: "revision_guidance".to_owned(),
                            value: sigil_kernel::UserInputAnswerValueV1::Text {
                                value: "Ask for the migration scope first.".to_owned(),
                            },
                        }],
                    },
                    permission_mode: None,
                },
            ),
        )
        .expect("guidance answer should start the revision");
    let revision_run_id = guidance_receipt
        .continuation_run_id
        .expect("guidance answer should expose the revision child run");

    let waiting_event = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match subscriber
                .recv_run_stream()
                .await
                .expect("waiting live event")
            {
                crate::sse::HttpRunStreamReceive::Event(event)
                    if event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .run_id
                        == revision_run_id
                        && matches!(
                            event
                                .run_event
                                .as_ref()
                                .expect("public event payload")
                                .event,
                            PublicRunEventKind::RunAwaitingUserInput { .. }
                        ) =>
                {
                    break event.run_event.expect("filtered public payload");
                }
                crate::sse::HttpRunStreamReceive::Event(_)
                | crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {}
            }
        }
    })
    .await
    .expect("revision should publish its durable waiting input event");
    let PublicRunEventKind::RunAwaitingUserInput {
        request_id,
        generation,
        request_hash,
    } = waiting_event.event
    else {
        panic!("waiting event kind was filtered above");
    };
    let waiting_sequence = waiting_event.sequence;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if registry
                .get_run(&revision_run_id)
                .expect("revision child should be registered")
                .status
                == HttpRunStatus::Paused
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiting attempt should project to a resumable registry pause");

    let waiting_projection = sigil_kernel::PlanReviewProjection::from_entries(
        &JsonlSessionStore::read_entries(&session.session_log_path).expect("waiting entries"),
    );
    let unmanaged_child_path = waiting_projection
        .latest_attempt(&review_id)
        .expect("waiting revision attempt")
        .child_session_ref
        .resolve(
            Path::new(&session.session_log_path)
                .parent()
                .expect("parent session directory"),
        );
    assert!(
        !unmanaged_child_path.exists(),
        "managed research must not create the parent-relative child log"
    );
    let cancellation_command = HttpCommandEnvelope::new(
        "revision-cancel-research-input",
        "client-1",
        &session.id,
        HttpUserInputDecisionRequest {
            generation,
            expected_request_hash: request_hash.clone(),
            decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
            permission_mode: None,
        },
    );
    if recover_accepted_child {
        // Cache an actual failed adapter command before simulating the durable crash prefix.
        // The normal command replay must remain cached; reopening must use session recovery.
        let guard = registry
            .reserve_durable_session_mutation(&session.durable_session_scope_id)
            .expect("idle waiting session mutation should be reservable");
        assert!(
            registry
                .user_input_decision_command(
                    &session.id,
                    &request_id,
                    cancellation_command.clone(),
                )
                .is_err(),
            "a competing session mutation must reject this adapter execution"
        );
        drop(guard);
        assert!(
            registry
                .user_input_decision_command(
                    &session.id,
                    &request_id,
                    cancellation_command.clone(),
                )
                .is_err(),
            "the same command must replay its cached failure without executing the driver"
        );
        let attempt = waiting_projection
            .latest_attempt(&review_id)
            .expect("exact waiting attempt");
        let pending = attempt
            .pending_user_input
            .as_ref()
            .expect("parent must reference the original child request");
        let writer = &driver
            .services
            .authority_composition()
            .expect("production must have its authority composition")
            .storage_writer;
        let lease = writer
            .acquire_named(
                sigil_runtime::managed_storage_writer::StorageWriterChannelV1::SessionLog,
                &format!("pr-{}-research-0", attempt.attempt_id.as_str()),
            )
            .expect("test must reopen the actual admitted research namespace");
        let mut child = sigil_kernel::Session::load_from_store(
            "custom",
            "gpt-4.1",
            JsonlSessionStore::new(lease.path().join("records.jsonl"))
                .expect("original managed child store"),
        )
        .expect("original managed child must reload");
        let child_receipt = sigil_kernel::accept_user_input_decision(
            &mut child,
            sigil_kernel::UserInputDecisionCommandV1 {
                identity: pending.identity.clone(),
                request_hash,
                command_id: sigil_kernel::UserInputCommandId::new("revision-cancel-research-input")
                    .expect("original command identity"),
                decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
            },
            current_unix_time_ms(),
        )
        .expect("child cancellation must be genuinely durable before parent settlement");
        assert!(matches!(
            child_receipt.request.resolution,
            Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
        ));
        drop(child);
        writer.finalize(lease).expect("child writer must settle");
        assert_eq!(
            sigil_kernel::PlanReviewProjection::from_entries(
                &JsonlSessionStore::read_entries(&session.session_log_path)
                    .expect("parent crash prefix"),
            )
            .latest_attempt(&review_id)
            .map(|attempt| attempt.status),
            Some(sigil_kernel::PlanReviewAttemptStatus::WaitingForInput),
            "the fixture must stop before parent terminal settlement"
        );
        let catalog = driver
            .session_lifecycle()
            .expect("reopen requires the same lifecycle service as production serve")
            .catalog()
            .expect("the managed session catalog must be readable");
        let catalog_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.session_id.as_deref() == Some(&session.durable_session_scope_id))
            .expect("the original managed parent session must appear in the catalog");
        let reopen_request = HttpSessionOpenRequest {
            session_ref: catalog_entry
                .session_ref
                .as_path()
                .to_str()
                .expect("UTF-8 session catalog reference")
                .to_owned(),
            session_id: session.durable_session_scope_id.clone(),
            label: None,
            recovery_binding: None,
        };
        // The shipping HTTP listener routes synchronous registry commands on a blocking worker.
        // Recovery directly re-enters the driver rather than the async application receipt cache.
        let reopen_registry = registry.clone();
        let opened =
            tokio::task::spawn_blocking(move || reopen_registry.open_session(reopen_request))
                .await
                .expect("the session reopen routing worker must not panic")
                .expect("actual session reopen must recover past the failed command cache");
        assert_eq!(opened.id, session.id);
    } else {
        let cancellation = registry
            .user_input_decision_command(&session.id, &request_id, cancellation_command)
            .expect("the exact research cancellation should finalize the revision");
        assert!(cancellation.continuation_run_id.is_none());
    }

    let terminal_event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match subscriber
                .recv_run_stream()
                .await
                .expect("revision cancellation terminal live event")
            {
                crate::sse::HttpRunStreamReceive::Event(event)
                    if event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .run_id
                        == revision_run_id
                        && matches!(
                            event
                                .run_event
                                .as_ref()
                                .expect("public event payload")
                                .event,
                            PublicRunEventKind::RunCancelled
                        ) =>
                {
                    break event.run_event.expect("filtered public payload");
                }
                crate::sse::HttpRunStreamReceive::Event(_)
                | crate::sse::HttpRunStreamReceive::StreamClosed { .. } => {}
            }
        }
    })
    .await
    .expect("cancelled revision should publish its terminal event");
    assert!(
        terminal_event.sequence > waiting_sequence,
        "cancel terminal must consume the next exact HTTP journal sequence"
    );
    assert_eq!(
        registry
            .get_run(&revision_run_id)
            .expect("revision child should remain registered")
            .status,
        HttpRunStatus::Cancelled
    );
    assert!(
        !driver
            .has_unresolved_user_input(&session)
            .expect("cancelled revision must clear current waiting admission")
    );
    let records = JsonlSessionStore::new(&session.session_log_path)
        .expect("session store")
        .read_event_records_writer()
        .expect("terminal records");
    let outbox_projection = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)
        .expect("cancel terminal bundle should validate");
    let terminal_outbox = outbox_projection
        .events_in_order()
        .into_iter()
        .find(|entry| {
            entry.run_id == revision_run_id
                && entry.sequence == terminal_event.sequence
                && matches!(&entry.event.event, PublicRunEventKind::RunCancelled)
        })
        .expect("cancelled revision should have exactly one terminal outbox");
    assert_eq!(terminal_outbox.event.sequence, terminal_event.sequence);
    assert_eq!(
        serde_json::to_value(&terminal_outbox.event)
            .expect("durable cancellation event should serialize"),
        serde_json::to_value(&terminal_event).expect("live cancellation event should serialize")
    );
    assert!(
        !outbox_projection
            .pending_for_adapter("http")
            .into_iter()
            .any(|entry| entry.public_event_id == terminal_outbox.public_event_id),
        "published cancellation must receive the HTTP delivery receipt"
    );
    assert_eq!(
        event_bus
            .replay_run_after(&session.durable_session_scope_id, &revision_run_id, None)
            .expect("cancelled revision should replay exact journal events")
            .into_iter()
            .filter(|event| {
                matches!(
                    event
                        .run_event
                        .as_ref()
                        .expect("public event payload")
                        .event,
                    PublicRunEventKind::RunAwaitingUserInput { .. }
                        | PublicRunEventKind::RunCancelled
                )
            })
            .map(|event| event
                .run_event
                .as_ref()
                .expect("public event payload")
                .sequence)
            .collect::<Vec<_>>(),
        vec![waiting_sequence, terminal_event.sequence]
    );
    assert!(
        !unmanaged_child_path.exists(),
        "cancellation must reopen the original managed child, not a parallel log"
    );
    fixture.abort();
}
