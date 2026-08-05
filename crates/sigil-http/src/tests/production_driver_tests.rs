use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use sigil_kernel::{
    AgentRole, AssistantMessageKind, CandidateCheck, CheckCommand, CheckDiscoverySource,
    CheckPromotion, CheckSpecRecordedEntry, CompletionCriteria, ControlEntry, EvidenceScope,
    JsonlSessionStore, NetworkEffect, PlanDraftCreatedEntry, ReadinessEvaluatedEntry,
    ReadinessEvaluation, RequiredAction, RunStatus, SessionLogEntry, SessionRef, TaskId,
    TaskPauseRequest, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus, ToolAccess, ToolApproval,
    ToolApprovalSessionGrantUnavailableReason, ToolApprovalSessionGrantUnavailableReasonCode,
    ToolArtifactDescriptorV1, ToolArtifactEncoding, ToolArtifactSensitivity, ToolArtifactStore,
    ToolCall, ToolCategory, ToolEffect, ToolPreviewCapability, ToolResult, ToolResultMeta,
    ToolResultRecordedV3, ToolSpec, VerificationPolicy, VerificationPolicyChangedEntry,
    VerificationProductAction, VerificationVerdict, VisibleCompletionState,
    build_workspace_snapshot, stable_workspace_id,
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
};

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
    let config = format!(
        r#"config_version = 2

[workspace]
root = "{workspace_root}"

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

fn write_reasoning_test_config(path: &std::path::Path) {
    let config = r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-test"
model = "deepseek-v4-flash"

[connections.deepseek-test]
label = "DeepSeek test"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#;
    std::fs::write(path, config).expect("reasoning test config should write");
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
    std::fs::write(
        temp.path().join("sigil.toml"),
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "replacement"
model = "gpt-replacement"

[connections.replacement]
label = "Replacement"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:2"
credential = { source = "none" }
"#,
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
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = { source = "none" }

[task]
enabled = true
"#,
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

fn append_durable_tool_artifact(
    session: &HttpSessionSnapshot,
    result: ToolResult,
) -> ToolArtifactDescriptorV1 {
    let session_store = JsonlSessionStore::new(std::path::Path::new(&session.session_log_path))
        .expect("session store should reopen");
    let artifact_store = ToolArtifactStore::for_session_store(&session_store);
    let (recorded, _display) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )
    .expect("tool result should capture");
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

#[tokio::test]
async fn production_driver_authorizes_artifact_reads_by_exact_session_and_hash() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let driver = production_queue_driver(&temp, "artifact-read");
    let session = production_queue_session_named(&temp, "artifact-owner");
    let store =
        ToolArtifactStore::for_session_path(std::path::Path::new(&session.session_log_path));
    let descriptor = append_durable_tool_artifact(
        &session,
        ToolResult::ok(
            "call-artifact",
            "shell",
            "line one\nline two\nline three\n",
            ToolResultMeta::default(),
        ),
    );
    assert!(
        store.source_event_id(&descriptor.artifact_ref).is_err(),
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

    let other_session = production_queue_session_named(&temp, "artifact-other");
    assert_eq!(
        driver.tool_artifact_page(&other_session, &request),
        Err(crate::HttpToolArtifactReadDriverError::Unavailable)
    );

    let binary_descriptor = store
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
        .expect("binary artifact should publish");
    let binary_descriptor = append_durable_tool_artifact(
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
    let blob_path = store
        .root()
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
    let session = production_queue_session_named(&temp, "artifact-binding-owner");
    let store =
        ToolArtifactStore::for_session_path(std::path::Path::new(&session.session_log_path));

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
        &session,
        ToolResult::ok(
            "call-bound",
            "shell",
            "durably projected",
            ToolResultMeta::default(),
        ),
    );
    let manifest_path = store
        .root()
        .join("refs")
        .join(format!("{}.json", descriptor.artifact_ref.artifact_id));
    let mut forged_descriptor = descriptor.clone();
    forged_descriptor.tool_name = "forged-tool".to_owned();
    std::fs::write(
        manifest_path,
        serde_json::to_vec(&forged_descriptor).expect("forged descriptor should encode"),
    )
    .expect("test should replace the descriptor manifest");
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
        store
            .resolve(&descriptor.artifact_ref)
            .expect("forged but well-formed physical descriptor should resolve")
            .tool_name,
        "forged-tool"
    );
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
    assert_eq!(display.through_session_stream_sequence, "7");
    assert_eq!(display.total_items, "2");
    assert_eq!(display.items.len(), 2);
    assert_eq!(display.items[1].display_order.session_stream_sequence, "3");
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
async fn production_driver_uses_shared_runtime_preparation_and_records_typed_failure() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let config_path = temp.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
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

    let terminal = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = subscriber
                .recv()
                .await
                .expect("production failure event should remain observable");
            if event.run_event.run_id == run.id
                && matches!(&event.run_event.event, PublicRunEventKind::RunFailed { .. })
            {
                break event;
            }
        }
    })
    .await
    .expect("preparation failure should terminate promptly");
    assert!(matches!(
        terminal.run_event.event,
        PublicRunEventKind::RunFailed { .. }
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
    .expect("terminal event should be followed by the terminal run projection");
    assert_eq!(projected_status, HttpRunStatus::Failed);
    assert!(session.session_log_path.ends_with(".jsonl"));
    let replay = event_bus
        .replay_run_after(&session.durable_session_scope_id, &run.id, None)
        .expect("typed preparation failure should be durable");
    assert!(matches!(
        replay.last().map(|event| &event.run_event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
}

#[test]
fn production_execution_without_a_public_terminal_is_closed_as_failed() {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let result = Err(anyhow!("private provider transport detail"));
    let mut recorder = Recorder::default();
    let terminal = ensure_application_execution_terminal(
        false,
        &result,
        "durable-session-1",
        "run-1",
        &mut recorder,
    )
    .expect("missing terminal should be recovered");

    assert_eq!(terminal, HttpRunTerminalOutcome::Failed);
    assert!(matches!(
        recorder.0.as_slice(),
        [PublicRunEvent {
            session_id,
            run_id,
            event: PublicRunEventKind::RunFailed { error },
            ..
        }] if session_id == "durable-session-1"
            && run_id == "run-1"
            && error == "run execution failed before its terminal status was published"
            && !error.contains("private provider")
    ));
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
    config_path: &std::path::Path,
    temp: &tempfile::TempDir,
    session: &HttpSessionSnapshot,
) -> sigil_kernel::PlanReviewId {
    let root_config = sigil_kernel::RootConfig::load(config_path).expect("config should load");
    let workspace_root = sigil_kernel::resolve_workspace_root(config_path, temp.path(), ".");
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
    let draft = PlanDraftCreatedEntry {
        plan_id: request.plan_id.clone(),
        schema_version: 2,
        source: sigil_kernel::PlanSourceRef::default(),
        plan_hash: "sha256:draft".to_owned(),
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
            suggested_checks: Vec::new(),
            risk: None,
            notes: Vec::new(),
        }],
        intent_proposal: None,
        target_paths: vec!["src/coordinator.rs".to_owned()],
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
        workspace_snapshot_id: Some(
            sigil_runtime::plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
                .expect("workspace snapshot should build")
                .expect("workspace snapshot id"),
        ),
        created_at_ms: 110,
    };
    sigil_runtime::PlanReviewCoordinator::commit_draft_from_child(
        &mut session_log,
        &draft,
        &request,
        120,
    )
    .expect("seed draft should commit");
    drop(session_log);
    review_id
}

#[tokio::test]
async fn production_revision_duplicate_registration_never_blocks_the_session() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");

    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

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
credential = { source = "none" }
"#,
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
    let review_id = seed_revision_session(&config_path, &temp, &session);

    // Pre-register the revision's deterministic child run id so spawn must reject the duplicate
    // AFTER `application_plan_decision` persisted the RevisionRequested decision.
    let attempt_1 = sigil_kernel::plan_review_attempt_id_for_review(&review_id);
    let attempt_2 = sigil_kernel::plan_review_attempt_id_for_revision(&review_id, &attempt_1);
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
            }),
        );

    let error = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-dup-1",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_1)
                        .as_str()
                        .to_owned(),
                    expected_plan_hash: "sha256:draft".to_owned(),
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
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

    // The durable `RevisionFailed` fact makes the failure recoverable: reloading the session
    // shows the failure instead of an unrecoverable `RevisionRequested` orphan.
    let store = JsonlSessionStore::new(&session.session_log_path).expect("reload store");
    let reloaded = sigil_kernel::Session::load_from_store("local-test", "gpt-4.1", store)
        .expect("revision session should reload");
    let original_plan_id = sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_1);
    let plan_projection = reloaded.plan_artifact_projection();
    let decision = plan_projection
        .latest_decision(&original_plan_id)
        .expect("durable revision failure decision");
    assert_eq!(
        decision.decision,
        sigil_kernel::PlanDecision::RevisionFailed,
        "the rejected revision must record a durable RevisionFailed fact"
    );

    // The original plan stays actionable: Save succeeds after the durable failure, proving the
    // plan decision recovery path rather than only the session slot.
    let save_receipt = registry
        .plan_decision_command(
            &session.id,
            HttpCommandEnvelope::new(
                "revision-recovery-1",
                "client-1",
                &session.id,
                HttpPlanDecisionRequest {
                    plan_id: original_plan_id.as_str().to_owned(),
                    expected_plan_hash: "sha256:draft".to_owned(),
                    action: HttpPlanDecisionAction::Save,
                    permission_grant: None,
                },
            ),
        )
        .expect("Save must succeed after a durable revision failure");
    assert_eq!(save_receipt.action, HttpPlanDecisionAction::Save);
}

#[tokio::test]
async fn production_revision_bind_failure_rolls_back_only_the_run_registration() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace should create");
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");

    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

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
credential = { source = "none" }
"#,
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

    // A different run already owns the session foreground slot: bind must fail after the
    // active-run registration succeeded, exercising the rollback path.
    registry
        .bind_supervised_session_run(&session.id, "other-foreground-run")
        .expect("pre-existing foreground slot should be claimable");
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
    let sessions = temp.path().join("sessions");
    std::fs::create_dir(&sessions).expect("session directory should create");

    // Local chat-completions fixture answering the revision plan review with a typed draft.
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
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = if request.contains("\"submit_plan_draft\"") {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"revision-draft-call\",\"type\":\"function\",\"function\":{\"name\":\"submit_plan_draft\",\"arguments\":\"{\\\"schema_version\\\":2,\\\"summary\\\":\\\"Revised coordinator migration\\\",\\\"steps\\\":[{\\\"step_id\\\":\\\"migrate_2\\\",\\\"title\\\":\\\"Revise migration\\\",\\\"role\\\":\\\"executor\\\",\\\"mode\\\":\\\"write\\\",\\\"isolation\\\":\\\"sequential_workspace_write\\\",\\\"target_paths\\\":[\\\"src/coordinator.rs\\\"]}],\\\"target_paths\\\":[\\\"src/coordinator.rs\\\"],\\\"suggested_checks\\\":[\\\"cargo test\\\"]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                    "data: [DONE]\n\n"
                )
            } else {
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"revised\"},\"finish_reason\":\"stop\"}]}\n\n",
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
    });

    let base_url = format!("http://{address}");
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[workspace]
root = "."

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

    let review_id = seed_revision_session(&config_path, &temp, &session);

    // Revise through the registry: the driver runs a supervised revision, publishes an explicit
    // terminal event, closes the SSE stream, and releases the session foreground slot.
    let receipt = registry
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
                    expected_plan_hash: "sha256:draft".to_owned(),
                    action: HttpPlanDecisionAction::Revise,
                    permission_grant: None,
                },
            ),
        )
        .expect("Revise decision should be accepted");
    assert_eq!(receipt.action, HttpPlanDecisionAction::Revise);
    let revision_run_id = receipt
        .revision_run_id
        .expect("Revise receipt must expose the revision run identity");

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
                    if event.run_event.run_id == revision_run_id
                        && matches!(
                            &event.run_event.event,
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
        terminal.run_event.event,
        PublicRunEventKind::RunFinished { .. }
    ));
    assert_eq!(closed_session, session.durable_session_scope_id);
    assert_eq!(closed_run, revision_run_id);

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
